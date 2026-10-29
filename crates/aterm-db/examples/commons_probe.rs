// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Development probe for automatic Commons replication between real machines.
//! `write <run-label> <JSON>` publishes one dedicated test table and appends a
//! test value. `read <run-label>` only opens its existing declaration and store;
//! it never publishes a declaration or joins an application table. A normal
//! aterm instance supplies discovery and repair for those two modes; neither
//! mode starts network workers.
//! `runs` lists retained test-run labels without exposing their values or
//! starting network workers, so an audit can reuse an existing marker.
//! `audit <run-label> <seconds>` listens for native LAN advertisements and
//! inspects authenticated operation inventories. Its protocol exchanges can
//! themselves repair data; the report distinguishes prior retention from repair
//! assisted by this inspector and never claims a remote application read.
//! `serve` uses the actual automatic owner with an explicit private policy copy.
//! `serve-live` explicitly pauses the old owner with guarded config restoration.
//! `receive-live` starts that same owner without a local marker or membership
//! Session, then reads existing received probe rows after stopping the owner.

#[cfg(unix)]
#[path = "commons_probe/live_guard.rs"]
mod live_guard;
#[cfg(unix)]
#[path = "commons_probe/serve.rs"]
mod serve;

#[cfg(unix)]
mod probe {
    use aterm_codec::hex::encode_lower;
    use aterm_db::{
        catalog::{Descriptor, Directory, State},
        replica::InventorySummary,
        table::{Session, store::Store},
    };
    use aterm_json::{Value, json};
    use aterm_secrets::{client::Paths, commons::bootstrap::Bootstrap};
    use std::{
        collections::BTreeSet,
        error::Error,
        io,
        path::Path,
        time::{Duration, Instant},
    };

    const APPLICATION: &str = "aterm-sync-probe";
    const ROLE: &str = "verification";
    const KIND: &str = "aterm-sync-probe-v1";
    const USAGE: &str = "usage: commons_probe runs | write <run-label> <JSON> | read <run-label> | audit <run-label> <seconds> | serve <run-label> <seconds> <private-config-path> | serve-live <run-label> <seconds> | receive-live <run-label> <seconds>";
    pub(super) type Result<T> = std::result::Result<T, Box<dyn Error>>;

    pub(super) struct LocalProbe {
        pub(super) value: Value,
        pub(super) table: [u8; 32],
        pub(super) known: InventorySummary,
        requested_operations: Vec<[u8; 32]>,
        runs: BTreeSet<String>,
    }

    impl LocalProbe {
        pub(super) fn has_requested_operations(&self) -> bool {
            !self.requested_operations.is_empty()
        }

        pub(super) fn matches(&self, report: &aterm_db::replica::Report) -> bool {
            report.table == Some(self.table)
                && report.local_known == Some(self.known)
                && report.local_offered == Some(self.known)
                && report.peer_known == Some(self.known)
        }
    }

    pub(super) fn prepare_existing(run: &str, config: &Path) -> Result<(Paths, LocalProbe)> {
        let paths = Paths::live()?;
        if !paths.dir.join("commons/control").is_dir() {
            return Err(io::Error::other("existing Commons custody is unavailable").into());
        }
        let baseline = read_existing_at(&paths, config, run)?;
        if baseline.requested_operations.is_empty() {
            return Err(io::Error::other("requested probe run has no local operation").into());
        }
        // No session or bootstrap escapes into the automatic network owner.
        Ok((paths, baseline))
    }

    /// Observe only previously enrolled catalog/table storage. No declaration,
    /// application directory or enrollment claim is created on this read path.
    pub(super) fn received_existing_at(
        paths: &Paths,
        config: &Path,
        run: &str,
    ) -> Result<Option<LocalProbe>> {
        if !paths.dir.join("commons/tables").try_exists()? {
            return Ok(None);
        }
        if !paths.dir.join("commons/control").is_dir() {
            return Err(io::Error::other("existing Commons custody is unavailable").into());
        }
        let bootstrap = Bootstrap::required_until(paths, Instant::now() + Duration::from_secs(2))?;
        let directory = Directory::new(paths, bootstrap.trust());
        let descriptor = Descriptor::application(bootstrap.trust(), APPLICATION, ROLE, 1)?;
        for scope in [directory.catalog_scope(), descriptor.scope] {
            if !directory.path(scope)?.try_exists()? {
                return Ok(None);
            }
        }
        let session = Session::observe(&bootstrap)?;
        snapshot_in(paths, config, &bootstrap, &session, "read", run, None).map(Some)
    }

    fn read_existing_at(paths: &Paths, config: &Path, run: &str) -> Result<LocalProbe> {
        let bootstrap = Bootstrap::required_until(paths, Instant::now() + Duration::from_secs(2))?;
        let session = Session::observe(&bootstrap)?;
        snapshot_in(paths, config, &bootstrap, &session, "read", run, None)
    }

    pub fn run() -> Result<Value> {
        let args = std::env::args_os()
            .skip(1)
            .map(|arg| arg.into_string().map_err(|_| io::Error::other(USAGE)))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        if let [mode, run, seconds, config] = args.as_slice()
            && mode == "serve"
        {
            return crate::serve::run(run, seconds.parse()?, Path::new(config));
        }
        if let [mode, run, seconds] = args.as_slice()
            && mode == "serve-live"
        {
            return crate::serve::run_live(run, seconds.parse()?);
        }
        if let [mode, run, seconds] = args.as_slice()
            && mode == "receive-live"
        {
            return crate::serve::receive_live(run, seconds.parse()?);
        }
        if let [mode, run, seconds] = args.as_slice()
            && mode == "audit"
        {
            return audit::run(run, seconds.parse()?);
        }
        let (mode, run, value) = match args.as_slice() {
            [mode] if mode == "runs" => (mode.as_str(), "", None),
            [mode, run, value] if mode == "write" => (
                mode.as_str(),
                run.as_str(),
                Some(aterm_json::from_str::<Value>(value)?),
            ),
            [mode, run] if mode == "read" => (mode.as_str(), run.as_str(), None),
            _ => return Err(io::Error::other(USAGE).into()),
        };
        snapshot(mode, run, value)
    }

    fn snapshot(mode: &str, run: &str, value: Option<Value>) -> Result<Value> {
        let paths = Paths::live()?;
        if matches!(mode, "read" | "runs") && !paths.dir.join("commons/control").is_dir() {
            return Err(io::Error::other("existing Commons custody is unavailable").into());
        }
        let config = aterm_types::dirs::aterm_config_path()
            .ok_or_else(|| io::Error::other("Commons configuration path is unavailable"))?;
        if matches!(mode, "read" | "runs") {
            let observed = read_existing_at(&paths, &config, run)?;
            return Ok(if mode == "runs" {
                runs_value(&observed)
            } else {
                observed.value
            });
        }
        // One local admission budget, with no check retry or network activity.
        let admission = Instant::now() + Duration::from_secs(2);
        let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, admission)?;
        let session = Session::from_admitted_journal(&bootstrap, journal)?;
        Ok(snapshot_in(&paths, &config, &bootstrap, &session, mode, run, value)?.value)
    }

    fn runs_value(observed: &LocalProbe) -> Value {
        json!({"mode":"runs","table":encode_lower(&observed.table),
            "runs":observed.runs.iter().collect::<Vec<_>>(),
            "known":{"count":observed.known.count,"sha256":encode_lower(&observed.known.fingerprint)}})
    }

    fn snapshot_in(
        paths: &Paths,
        config: &Path,
        bootstrap: &Bootstrap,
        session: &Session<'_>,
        mode: &str,
        run: &str,
        value: Option<Value>,
    ) -> Result<LocalProbe> {
        let descriptor = Descriptor::application(bootstrap.trust(), APPLICATION, ROLE, 1)?;
        let directory = Directory::new(paths, bootstrap.trust());
        let written = if let Some(value) = value {
            directory.publish(session, &descriptor)?;
            let mut store = Store::open(
                session,
                &directory.path(descriptor.scope)?,
                descriptor.scope,
            )?;
            let payload = aterm_json::to_vec(&json!({"kind":KIND,"run":run,"value":value}))?;
            Some(store.commit_catalog(session, &payload)?.id)
        } else {
            None
        };
        // This observation refuses an absent catalog instead of enrolling it.
        // The reader also refuses an absent application store below.
        let snapshot = directory.snapshot_existing_with_config(session, config)?;
        let table = descriptor.scope.digest();
        let entry = snapshot
            .entries
            .get(&table)
            .ok_or_else(|| io::Error::other("test table declaration has not arrived"))?;
        if entry.state != State::Subscribed
            || entry
                .declarations
                .iter()
                .any(|declaration| declaration.descriptor != descriptor)
        {
            return Err(
                io::Error::other("test table declaration is conflicted or excluded").into(),
            );
        }
        let store = Store::open(
            session,
            &directory.path(descriptor.scope)?,
            descriptor.scope,
        )?;
        let records = store.catalog(session)?;
        let total_records = records.len();
        let mut operations = Vec::with_capacity(records.len());
        let mut requested_operations = Vec::new();
        let mut runs = BTreeSet::new();
        for record in records {
            let payload: Value = aterm_json::from_slice(record.payload())?;
            if payload.get("kind").and_then(Value::as_str) != Some(KIND)
                || payload.get("run").and_then(Value::as_str).is_none()
                || payload.get("value").is_none()
            {
                return Err(io::Error::other("unexpected data in dedicated probe table").into());
            }
            let label = payload["run"].as_str().expect("validated run label");
            runs.insert(label.to_owned());
            if label != run {
                continue;
            }
            let receipt = store
                .receipt(session, &record.header().id)?
                .ok_or_else(|| io::Error::other("test operation has no local receipt"))?;
            requested_operations.push(record.header().id);
            operations.push(json!({
                "operation":encode_lower(&record.header().id),
                "actor":encode_lower(&record.header().actor),
                "authored_at_ms":record.authored_ms(),
                "received_at_ms":receipt.received_ms,
                "local_offset":receipt.offset,
                "test_value":payload["value"],
            }));
        }
        let known: BTreeSet<_> = store.inventory(session)?.copied().collect();
        let offered: BTreeSet<_> = store.replication_inventory(session)?.copied().collect();
        if requested_operations
            .iter()
            .any(|id| !known.contains(id) || !offered.contains(id))
        {
            return Err(io::Error::other(
                "requested probe operation is not eligible for replication",
            )
            .into());
        }
        let known = InventorySummary::of(&known);
        let offered = InventorySummary::of(&offered);
        let value = json!({
            "mode":mode,"run":run,"application":APPLICATION,"role":ROLE,
            "network":encode_lower(&bootstrap.trust().network),
            "table":encode_lower(&table),"local_actor":encode_lower(&session.actor()),
            "written_operation":written.map(|id|encode_lower(&id)),
            "declarations":entry.declarations.len(),"records":operations.len(),
            "total_table_records":total_records,"operations":operations,
            "requested_operation_ids":requested_operations.iter().map(|id|encode_lower(id)).collect::<Vec<_>>(),
            "baseline_known":{"count":known.count,"sha256":encode_lower(&known.fingerprint)},
            "baseline_offered":{"count":offered.count,"sha256":encode_lower(&offered.fingerprint)},
        });
        Ok(LocalProbe {
            value,
            table,
            known,
            requested_operations,
            runs,
        })
    }

    #[cfg(test)]
    mod receiver_tests {
        use super::*;
        use aterm_secrets::client;

        #[test]
        fn receiver_read_authenticates_actual_rows_without_seeding_missing_storage() {
            let root = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(&root.path().join("receiver"));
            let config = paths.home.join("aterm.toml");
            assert!(
                received_existing_at(&paths, &config, "native-proof")
                    .unwrap()
                    .is_none()
            );
            assert!(!paths.home.exists(), "an absent read creates no custody");

            let source = root.path().join("team.vault");
            client::create(&paths, source.to_str().unwrap(), None, Some("receiver")).unwrap();
            assert!(
                received_existing_at(&paths, &config, "native-proof")
                    .unwrap()
                    .is_none()
            );
            assert!(!paths.dir.join("commons/tables").exists());
            assert!(!paths.dir.join("commons/table-claims").exists());
            let (bootstrap, journal) = Bootstrap::required_with_journal_until(
                &paths,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
            let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
            let written = snapshot_in(
                &paths,
                &config,
                &bootstrap,
                &session,
                "write",
                "native-proof",
                Some(json!({"message":"actual dedicated test value"})),
            )
            .unwrap();
            drop(session);
            drop(bootstrap);
            let read = received_existing_at(&paths, &config, "native-proof")
                .unwrap()
                .unwrap();
            assert!(read.has_requested_operations());
            assert_eq!(read.value["operations"], written.value["operations"]);
            assert_eq!(
                read.value["requested_operation_ids"],
                written.value["requested_operation_ids"]
            );
            assert_eq!(read.value["declarations"], json!(1));
            assert_eq!(read.value["written_operation"], Value::Null);
            let again = received_existing_at(&paths, &config, "native-proof")
                .unwrap()
                .unwrap();
            assert_eq!(
                again.value, read.value,
                "readback adds no declaration or operation"
            );
        }

        #[test]
        fn receiver_read_observes_committed_rows_while_writers_remain_open() {
            let root = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(&root.path().join("receiver"));
            let config = paths.home.join("aterm.toml");
            let source = root.path().join("team.vault");
            client::create(&paths, source.to_str().unwrap(), None, Some("receiver")).unwrap();
            let (bootstrap, journal) = Bootstrap::required_with_journal_until(
                &paths,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
            let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
            let written = snapshot_in(
                &paths,
                &config,
                &bootstrap,
                &session,
                "write",
                "active-owner",
                Some(json!({"message":"retained beside an active writer"})),
            )
            .unwrap();
            snapshot_in(
                &paths,
                &config,
                &bootstrap,
                &session,
                "write",
                "another-owner",
                Some(json!({"message":"another private marker value"})),
            )
            .unwrap();
            let directory = Directory::new(&paths, bootstrap.trust());
            let descriptor =
                Descriptor::application(bootstrap.trust(), APPLICATION, ROLE, 1).unwrap();
            let catalog_path = directory.path(directory.catalog_scope()).unwrap();
            let table_path = directory.path(descriptor.scope).unwrap();
            let _catalog = Store::open(&session, &catalog_path, directory.catalog_scope()).unwrap();
            let _table = Store::open(&session, &table_path, descriptor.scope).unwrap();
            let before_catalog = std::fs::read(catalog_path.join("table.astream")).unwrap();
            let before_table = std::fs::read(table_path.join("table.astream")).unwrap();

            let read = received_existing_at(&paths, &config, "active-owner")
                .expect("reading committed rows needs no writer admission")
                .expect("the retained table exists");
            assert_eq!(read.value["operations"], written.value["operations"]);
            assert_eq!(read.known.count, 2);
            let labels = runs_value(&read);
            assert_eq!(labels["runs"], json!(["active-owner", "another-owner"]));
            assert_eq!(labels["known"]["count"], json!(2));
            assert!(!labels.to_string().contains("private marker value"));
            assert!(!labels.to_string().contains("retained beside"));
            assert_eq!(
                std::fs::read(catalog_path.join("table.astream")).unwrap(),
                before_catalog
            );
            assert_eq!(
                std::fs::read(table_path.join("table.astream")).unwrap(),
                before_table
            );
        }

        #[test]
        fn source_report_recognizes_a_later_inventory_without_accepting_incomplete_evidence() {
            use aterm_db::{control, replica, runtime::RepairRound};

            let root = aterm_tempfile::tempdir().unwrap();
            let paths = Paths::for_home(&root.path().join("source"));
            let config = paths.home.join("aterm.toml");
            let source = root.path().join("team.vault");
            client::create(&paths, source.to_str().unwrap(), None, Some("source")).unwrap();
            let (bootstrap, journal) = Bootstrap::required_with_journal_until(
                &paths,
                Instant::now() + Duration::from_secs(60),
            )
            .unwrap();
            let session = Session::from_admitted_journal(&bootstrap, journal).unwrap();
            let baseline = snapshot_in(
                &paths,
                &config,
                &bootstrap,
                &session,
                "write",
                "prepared",
                Some(json!({"sequence":1})),
            )
            .unwrap();
            snapshot_in(
                &paths,
                &config,
                &bootstrap,
                &session,
                "write",
                "during-owner",
                Some(json!({"sequence":2})),
            )
            .unwrap();
            let actor = session.actor();
            drop(session);
            drop(bootstrap);
            let current = received_existing_at(&paths, &config, "prepared")
                .unwrap()
                .unwrap();
            assert_eq!((baseline.known.count, current.known.count), (1, 2));
            let round = RepairRound {
                target: None,
                actor: Some(actor),
                exchange_started: true,
                cancelled: false,
                phase: Some(control::Phase::Complete),
                control: Ok(control::Report::default()),
                membership: None,
                membership_error: None,
                application: Some(replica::Report {
                    table: Some(current.table),
                    local_known: Some(current.known),
                    local_offered: Some(current.known),
                    peer_known: Some(current.known),
                    ..Default::default()
                }),
                application_error: None,
                completed_at_ms: 1,
                dropped_before: 0,
            };
            let observe = |current: Option<&LocalProbe>, round, dropped| {
                crate::serve::source_evidence(&baseline, current, &[round], dropped)
            };
            let evidence = observe(Some(&current), round.clone(), 0);
            assert_eq!(evidence["outcome"], json!("automatic-owner-retention"));
            assert_eq!(evidence["verified_rounds"], json!(0));
            assert_eq!(evidence["post_stop_verified_rounds"], json!(1));
            assert_eq!(evidence["verification_basis"], json!("post-stop"));
            assert_eq!(evidence["baseline_changed"], json!(true));
            assert_eq!(evidence["post_stop_probe"]["total_table_records"], json!(2));

            for mutation in 0..11 {
                let mut invalid = round.clone();
                match mutation {
                    0 => invalid.cancelled = true,
                    1 => invalid.exchange_started = false,
                    2 => invalid.actor = None,
                    3 => invalid.control = Err("control failed".into()),
                    4 => invalid.application_error = Some("application failed".into()),
                    5 => invalid.application = None,
                    6 => invalid.application.as_mut().unwrap().table = Some([99; 32]),
                    7 => invalid.application.as_mut().unwrap().local_known = Some(baseline.known),
                    8 => invalid.application.as_mut().unwrap().local_offered = Some(baseline.known),
                    9 => invalid.application.as_mut().unwrap().peer_known = Some(baseline.known),
                    10 => invalid.membership_error = Some("membership failed".into()),
                    _ => unreachable!(),
                }
                assert_eq!(
                    observe(Some(&current), invalid, 0)["outcome"],
                    json!("not-established")
                );
            }
            assert_eq!(
                observe(Some(&current), round.clone(), 1)["outcome"],
                json!("not-established")
            );
            assert_eq!(
                observe(None, round.clone(), 0)["outcome"],
                json!("not-established")
            );
            let absent = received_existing_at(&paths, &config, "absent-run")
                .unwrap()
                .unwrap();
            assert!(!absent.has_requested_operations());
            let missing = observe(Some(&absent), round, 0);
            assert_eq!(missing["outcome"], json!("not-established"));
            assert_eq!(missing["post_stop_probe"]["records"], json!(0));
            assert_eq!(missing["post_stop_probe"]["total_table_records"], json!(2));
        }

        #[test]
        fn receiver_preflight_accepts_vault_approved_pending_membership_without_table_creation() {
            let root = aterm_tempfile::tempdir().unwrap();
            let issuer = Paths::for_home(&root.path().join("issuer"));
            let receiver = Paths::for_home(&root.path().join("receiver"));
            let source = root.path().join("team.vault");
            client::create(&issuer, source.to_str().unwrap(), None, Some("issuer")).unwrap();
            let recipient = client::machine_identity(&receiver).unwrap().recipient();
            let admin = client::admin_open(&issuer).unwrap();
            let mut document = admin.doc.clone();
            document
                .add_machine("receiver", &recipient.to_bech32())
                .unwrap();
            client::admin_publish_approved(&issuer, &admin, &document, &[recipient.to_bech32()])
                .unwrap();
            let pin = client::config(&issuer).unwrap().unwrap().signer.unwrap();
            client::join(
                &receiver,
                &client::JoinOptions {
                    source: format!(
                        "{}#{}",
                        source.display(),
                        aterm_secrets::b64::encode_padded(&pin)
                    ),
                    host: Some("receiver".into()),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: client::JoinPassword::Given(None),
                    fetched: None,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
            .unwrap();
            let pending = Bootstrap::cached(&receiver).unwrap().unwrap();
            assert!(pending.witness().unwrap().is_none());
            assert!(
                Session::open(&pending).is_err(),
                "the fixture has no current Commons membership"
            );
            assert!(
                received_existing_at(&receiver, &receiver.home.join("aterm.toml"), "native-proof")
                    .unwrap()
                    .is_none()
            );
            assert!(!receiver.dir.join("commons/tables").exists());
            assert!(!receiver.dir.join("commons/table-claims").exists());
        }
    }

    mod audit {
        use super::*;
        use astream_wire::sync::Actor;
        use aterm_db::{
            control::{Endpoint, Repair, Report as ControlReport, Side},
            replica::{InventorySummary, Report},
        };
        use aterm_net::lan::{Cancellation, Connection, Listener, Multicast, Scope};
        use std::{
            collections::{BTreeMap, BTreeSet},
            net::{IpAddr, Ipv4Addr, SocketAddr},
            sync::{Arc, Mutex},
            thread::{self, JoinHandle},
            time::{Duration, Instant},
        };

        const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 31, 84);
        const PORT: u16 = 41384;
        const MAX_PEERS: usize = 64;
        // Serial exchanges make prior-inspector accounting sound even when
        // several advertised endpoints authenticate as the same machine.
        const MAX_WORKERS: usize = 1;
        const MAX_ROUNDS: usize = 512;

        #[derive(Default)]
        struct Effects {
            control_sent: usize,
            control_received: usize,
            application_sent: usize,
            application_received: usize,
            keys_sent: usize,
            keys_received: usize,
            conflicts_sent: usize,
            conflicts_received: usize,
            deferred: usize,
            unaccounted_exchange: bool,
        }
        impl Effects {
            fn assisted(&self) -> bool {
                self.control_sent != 0
                    || self.control_received != 0
                    || self.application_sent != 0
                    || self.application_received != 0
                    || self.keys_sent != 0
                    || self.keys_received != 0
                    || self.conflicts_sent != 0
                    || self.conflicts_received != 0
                    || self.deferred != 0
                    || self.unaccounted_exchange
            }
            fn control(&mut self, report: &ControlReport) {
                self.control_sent += report.sent;
                self.control_received += report.received;
                self.deferred += report.deferred;
            }
            fn application(&mut self, report: &Report) {
                self.application_sent += report.sent;
                self.application_received += report.received;
                self.keys_sent += report.keys_sent;
                self.keys_received += report.keys_received;
                self.conflicts_sent += report.conflicts_sent;
                self.conflicts_received += report.conflicts_received;
                self.deferred += report.deferred;
            }
            fn accumulate(&mut self, other: &Self) {
                self.control_sent += other.control_sent;
                self.control_received += other.control_received;
                self.application_sent += other.application_sent;
                self.application_received += other.application_received;
                self.keys_sent += other.keys_sent;
                self.keys_received += other.keys_received;
                self.conflicts_sent += other.conflicts_sent;
                self.conflicts_received += other.conflicts_received;
                self.deferred += other.deferred;
                self.unaccounted_exchange |= other.unaccounted_exchange;
            }
            fn value(&self) -> Value {
                json!({
                    "control_sent":self.control_sent,"application_sent":self.application_sent,
                    "control_received":self.control_received,"application_received":self.application_received,
                    "keys_sent":self.keys_sent,"keys_received":self.keys_received,
                    "conflicts_sent":self.conflicts_sent,"deferred":self.deferred,
                    "conflicts_received":self.conflicts_received,
                    "unaccounted_exchange":self.unaccounted_exchange,
                })
            }
        }
        struct Peer {
            repair: Option<Repair>,
            next: Instant,
            previous_unaccounted: bool,
        }
        #[derive(Default)]
        struct Channel {
            cancellation: Option<Cancellation>,
            stopped: bool,
        }
        struct Finished {
            address: SocketAddr,
            repair: Repair,
            exchange_started: bool,
            cancelled: bool,
            result: std::result::Result<ControlReport, String>,
        }
        struct Worker {
            channel: Arc<Mutex<Channel>>,
            task: JoinHandle<Finished>,
        }
        impl Worker {
            fn start(
                endpoint: Arc<Endpoint>,
                address: SocketAddr,
                mut repair: Repair,
                until: Instant,
            ) -> io::Result<Self> {
                let channel = Arc::new(Mutex::new(Channel::default()));
                let connection_state = channel.clone();
                let task = thread::Builder::new()
                    .name("commons-probe-audit".into())
                    .spawn(move || {
                        let mut exchange_started = false;
                        let result = (|| -> Result<ControlReport> {
                            let remaining = until.saturating_duration_since(Instant::now());
                            if remaining.is_zero() {
                                return Err(io::Error::other("audit deadline reached").into());
                            }
                            let connection = Connection::connect(
                                address,
                                Scope::Lan,
                                remaining.min(Duration::from_secs(3)),
                            )?;
                            let cancellation = connection.cancellation()?;
                            {
                                let mut state =
                                    connection_state.lock().unwrap_or_else(|p| p.into_inner());
                                if state.stopped || Instant::now() >= until {
                                    cancellation.cancel();
                                    return Err(io::Error::other("audit deadline reached").into());
                                }
                                state.cancellation = Some(cancellation);
                            }
                            exchange_started = true;
                            Ok(endpoint.exchange(connection, Side::Initiator, &mut repair)?)
                        })()
                        .map_err(|error| error.to_string());
                        let cancelled = {
                            let mut state =
                                connection_state.lock().unwrap_or_else(|p| p.into_inner());
                            state.cancellation = None;
                            state.stopped
                        };
                        Finished {
                            address,
                            repair,
                            exchange_started,
                            cancelled,
                            result,
                        }
                    })?;
                Ok(Self { channel, task })
            }
            fn cancel(&self) {
                let mut state = self.channel.lock().unwrap_or_else(|p| p.into_inner());
                state.stopped = true;
                if let Some(cancellation) = &state.cancellation {
                    cancellation.cancel();
                }
            }
        }

        fn advertised(wire: &[u8], source: SocketAddr) -> Option<SocketAddr> {
            if wire.len() != 38 || !wire.starts_with(b"ACH2") {
                return None;
            }
            let IpAddr::V4(ip) = source.ip() else {
                return None;
            };
            if ip.octets()[0] == 0 || ip.octets()[0] >= 224 || ip.is_broadcast() || ip.is_loopback()
            {
                return None;
            }
            let port = u16::from_be_bytes([wire[36], wire[37]]);
            (port != 0).then_some(SocketAddr::from((ip, port)))
        }
        fn inventory(value: Option<InventorySummary>) -> Value {
            value.map_or(Value::Null, |summary| {
                json!({"count":summary.count,"sha256":encode_lower(&summary.fingerprint)})
            })
        }
        fn classification(
            report: Option<Report>,
            table: &[u8; 32],
            expected: InventorySummary,
            assisted: bool,
        ) -> &'static str {
            let Some(report) = report else {
                return "application-not-observed";
            };
            if report.table.as_ref() != Some(table) {
                return "other-table";
            }
            let (Some(local), Some(offered), Some(peer)) =
                (report.local_known, report.local_offered, report.peer_known)
            else {
                return "probe-inventory-unavailable";
            };
            if local.count == 0 {
                return "probe-empty-before-repair";
            }
            if local != expected {
                return "probe-baseline-changed-before-repair";
            }
            if local != peer {
                return "probe-not-converged-before-repair";
            }
            if local != offered {
                return "probe-retention-withheld-locally";
            }
            let mut current = Effects::default();
            current.application(&report);
            if assisted || current.assisted() {
                "matching-inventory-after-inspection"
            } else {
                "preexisting-retention-without-inspector-repair"
            }
        }
        fn observe(
            finished: Finished,
            table: &[u8; 32],
            expected: InventorySummary,
            peers: &mut BTreeMap<SocketAddr, Peer>,
            effects: &mut BTreeMap<Actor, Effects>,
            global: &mut Effects,
            rounds: &mut Vec<Value>,
        ) {
            let peer = peers.get_mut(&finished.address).expect("scheduled peer");
            let application = finished
                .exchange_started
                .then(|| finished.repair.application())
                .flatten();
            let application_error = finished
                .exchange_started
                .then(|| finished.repair.application_error())
                .flatten();
            // Failed control exchanges can already have sent records. Retain
            // that uncertainty under the current authenticated identity, not
            // merely this endpoint; another advertisement may name the same peer.
            let actor = finished
                .exchange_started
                .then(|| finished.repair.authenticated_actor())
                .flatten();
            let authenticated = actor.is_some();
            let failed = finished.result.is_err();
            let unaccounted = peer.previous_unaccounted
                || failed
                || application_error.is_some()
                || finished.cancelled;
            let prior = actor.map(|actor| effects.entry(actor).or_default());
            let mut before = Value::Null;
            let global_before = global.value();
            // Sending or receiving control, keys or data can enable an ordinary
            // runtime to spread a marker. Per-peer zero sends do not exclude
            // that causal path. Count both directions, including this round.
            let mut current = Effects {
                unaccounted_exchange: unaccounted,
                ..Effects::default()
            };
            if let Ok(control) = &finished.result {
                current.control(control);
            }
            if let Some(application) = application {
                current.application(&application);
            }
            let mut assisted = current.assisted() || global.assisted();
            let mut direct_assistance = current.assisted();
            if let Some(prior) = prior {
                before = prior.value();
                assisted |= prior.assisted();
                direct_assistance |= prior.assisted();
                prior.accumulate(&current);
            }
            global.accumulate(&current);
            let control = match &finished.result {
                Ok(report) => json!({
                    "sent":report.sent,"received":report.received,"deferred":report.deferred,
                }),
                Err(error) => json!({"error":error}),
            };
            let application_value = application.map_or(Value::Null, |report| {
                json!({
                    "table":report.table.map(|id|encode_lower(&id)),
                    "local_known":inventory(report.local_known),
                    "local_offered":inventory(report.local_offered),
                    "peer_known":inventory(report.peer_known),
                    "sent":report.sent,"received":report.received,"deferred":report.deferred,
                    "keys_sent":report.keys_sent,"keys_received":report.keys_received,
                    "conflicts_sent":report.conflicts_sent,
                    "conflicts_received":report.conflicts_received,
                })
            });
            rounds.push(json!({
                "endpoint":finished.address.to_string(),"authenticated_this_round":authenticated,
                "exchange_started":finished.exchange_started,
                "cancelled":finished.cancelled,
                "phase":if finished.exchange_started {
                    finished.repair.phase()
                } else {
                    aterm_db::control::Phase::Connecting
                }.label(),
                "actor":actor.map(|id|encode_lower(&id)),
                "last_authenticated_actor":finished.repair.actor().map(|id|encode_lower(&id)),
                "inspector_effects_before_round":before,
                "global_inspector_effects_before_round":global_before,
                "direct_inspector_assistance":direct_assistance,
                "classification":classification(application,table,expected,assisted),
                "control":control,"application":application_value,
                "application_error":application_error,
            }));
            peer.previous_unaccounted = unaccounted;
            peer.repair = Some(finished.repair);
            peer.next = Instant::now() + Duration::from_secs(3);
        }

        pub(super) fn run(run: &str, seconds: u64) -> Result<Value> {
            if !(1..=60).contains(&seconds) {
                return Err(io::Error::other("audit seconds must be between 1 and 60").into());
            }
            let started = Instant::now();
            let paths = Paths::live()?;
            if !paths.dir.join("commons/control").is_dir() {
                return Err(io::Error::other("existing Commons custody is unavailable").into());
            }
            let config = aterm_types::dirs::aterm_config_path()
                .ok_or_else(|| io::Error::other("Commons configuration path is unavailable"))?;
            let admission = Instant::now() + Duration::from_secs(2);
            let (bootstrap, journal) = Bootstrap::required_with_journal_until(&paths, admission)?;
            let session = Session::from_admitted_journal(&bootstrap, journal)?;
            let local_probe =
                snapshot_in(&paths, &config, &bootstrap, &session, "read", run, None)?;
            if local_probe.requested_operations.is_empty() {
                return Err(io::Error::other("requested probe run has no local operation").into());
            }
            let table = local_probe.table;
            let actor = session.actor();
            let expected = local_probe.known;
            let endpoint = Arc::new(
                Endpoint::from_session(&session)?
                    .with_commons_from_session(&session, paths, config)?,
            );
            // The local payload, marker IDs, canonical baseline and detached
            // endpoint came from one consistent locked session. None of its
            // locks may cross discovery or an authenticated network exchange.
            drop(session);
            drop(bootstrap);
            let mut sockets = Vec::new();
            let mut discovery_errors = Vec::new();
            let addresses = Listener::candidates()?;
            let own_addresses: BTreeSet<_> = addresses.iter().map(SocketAddr::ip).collect();
            for address in addresses {
                let IpAddr::V4(source) = address.ip() else {
                    continue;
                };
                match Multicast::bind(source, GROUP, PORT, Scope::Lan) {
                    Ok(socket) => sockets.push(socket),
                    Err(error) => discovery_errors
                        .push(json!({"source":source.to_string(),"error":error.to_string()})),
                }
            }
            let interfaces = sockets.len();
            if interfaces == 0 {
                return Ok(json!({
                    "mode":"audit","run":run,"table":encode_lower(&table),
                    "local_actor":encode_lower(&actor),"seconds":seconds,
                    "outcome":"unavailable","interfaces":0,"rounds":[],
                    "discovered_endpoints":0,"discovery_errors":discovery_errors,
                    "reason":"No native discovery socket could bind an eligible physical LAN interface",
                }));
            }
            let mut peers = BTreeMap::<SocketAddr, Peer>::new();
            let mut effects = BTreeMap::new();
            let mut global = Effects::default();
            let mut workers: Vec<Worker> = Vec::new();
            let mut rounds = Vec::new();
            let mut cursor = None;
            let preparation_ms = started.elapsed().as_millis();
            let network_started = Instant::now();
            let until = network_started + Duration::from_secs(seconds);
            while Instant::now() < until && rounds.len() < MAX_ROUNDS {
                for socket in &sockets {
                    for _ in 0..16 {
                        match socket.receive() {
                            Ok(Some((wire, source))) => {
                                if own_addresses.contains(&source.ip()) {
                                    continue;
                                }
                                let Some(address) = advertised(&wire, source) else {
                                    continue;
                                };
                                if !peers.contains_key(&address)
                                    && peers.len() < MAX_PEERS
                                    && peers.keys().filter(|p| p.ip() == address.ip()).count() < 2
                                {
                                    peers.insert(
                                        address,
                                        Peer {
                                            repair: Some(Repair::default()),
                                            next: Instant::now(),
                                            previous_unaccounted: false,
                                        },
                                    );
                                }
                            }
                            Ok(None) => break,
                            Err(error) => {
                                if discovery_errors.len() < MAX_PEERS {
                                    discovery_errors.push(json!({"error":error.to_string()}));
                                }
                                break;
                            }
                        }
                    }
                }
                let mut index = 0;
                while index < workers.len() {
                    if workers[index].task.is_finished() {
                        let worker = workers.swap_remove(index);
                        match worker.task.join() {
                            Ok(finished) => observe(
                                finished,
                                &table,
                                expected,
                                &mut peers,
                                &mut effects,
                                &mut global,
                                &mut rounds,
                            ),
                            Err(_) => {
                                global.unaccounted_exchange = true;
                                discovery_errors.push(json!({"error":"audit worker panicked"}))
                            }
                        }
                    } else {
                        index += 1;
                    }
                }
                let available = MAX_WORKERS.saturating_sub(workers.len());
                let ready: Vec<_> =
                    peers
                        .iter()
                        .filter(|(address, _)| cursor.is_none_or(|cursor| **address > cursor))
                        .chain(peers.iter().filter(|(address, _)| {
                            cursor.is_some_and(|cursor| **address <= cursor)
                        }))
                        .filter(|(_, peer)| peer.repair.is_some() && peer.next <= Instant::now())
                        .take(available)
                        .map(|(address, _)| *address)
                        .collect();
                for address in ready {
                    cursor = Some(address);
                    let repair = peers.get_mut(&address).unwrap().repair.take().unwrap();
                    match Worker::start(endpoint.clone(), address, repair, until) {
                        Ok(worker) => workers.push(worker),
                        Err(error) => discovery_errors.push(json!({"error":error.to_string()})),
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
            for worker in &workers {
                worker.cancel();
            }
            for worker in workers {
                match worker.task.join() {
                    Ok(finished) => observe(
                        finished,
                        &table,
                        expected,
                        &mut peers,
                        &mut effects,
                        &mut global,
                        &mut rounds,
                    ),
                    Err(_) => {
                        global.unaccounted_exchange = true;
                        discovery_errors.push(json!({"error":"audit worker panicked"}));
                    }
                }
            }
            let verified_rounds = rounds
                .iter()
                .filter(|round| {
                    round["classification"].as_str()
                        == Some("preexisting-retention-without-inspector-repair")
                })
                .count();
            let assisted_rounds = rounds
                .iter()
                .filter(|round| {
                    round["classification"].as_str() == Some("matching-inventory-after-inspection")
                })
                .count();
            let control_failed_rounds = rounds
                .iter()
                .filter(|round| round["control"].get("error").is_some())
                .count();
            let application_failed_rounds = rounds
                .iter()
                .filter(|round| !round["application_error"].is_null())
                .count();
            let outcome = if verified_rounds > 0 {
                "preexisting-retention"
            } else if assisted_rounds > 0 {
                "assisted-retention"
            } else {
                "not-established"
            };
            Ok(json!({
                "mode":"audit","run":run,"table":encode_lower(&table),
                "local_actor":encode_lower(&actor),"seconds":seconds,
                "elapsed_ms":started.elapsed().as_millis(),"preparation_ms":preparation_ms,
                "audit_elapsed_ms":network_started.elapsed().as_millis(),"interfaces":interfaces,
                "outcome":outcome,"local_probe":local_probe.value,
                "verified_rounds":verified_rounds,"assisted_rounds":assisted_rounds,
                "control_failed_rounds":control_failed_rounds,"application_failed_rounds":application_failed_rounds,
                "discovered_endpoints":peers.len(),"rounds":rounds,
                "discovery_errors":discovery_errors,
                "global_inspector_effects":global.value(),
                "effects":effects.iter().map(|(actor,effects)|json!({
                    "actor":encode_lower(actor),"inspector_effects":effects.value(),
                })).collect::<Vec<_>>(),
                "scope":"Authenticated retention inventories matching the prepared requested-run baseline, before each round's positive repair. Assistance accounting covers this invocation only; earlier audits may have repaired data. Remote offered sets, decryption and application/provider readiness are not observed. Inspector-assisted and unaccounted rounds do not prove automatic synchronization.",
            }))
        }

        #[cfg(test)]
        mod tests {
            use super::*;

            #[test]
            fn discovery_dials_only_a_bounded_native_service_advertisement() {
                let source = SocketAddr::from(([192, 168, 1, 23], PORT));
                let mut wire = b"ACH2".to_vec();
                wire.extend_from_slice(&[7; 32]);
                wire.extend_from_slice(&43210u16.to_be_bytes());
                assert_eq!(
                    advertised(&wire, source),
                    Some(SocketAddr::from(([192, 168, 1, 23], 43210)))
                );
                assert!(advertised(&wire[..37], source).is_none());
                assert!(advertised(&wire, SocketAddr::from(([127, 0, 0, 1], PORT))).is_none());
                wire[0] = b'X';
                assert!(advertised(&wire, source).is_none());
                wire[0] = b'A';
                wire[36..].copy_from_slice(&[0, 0]);
                assert!(advertised(&wire, source).is_none());
            }

            #[test]
            fn positive_inventory_evidence_requires_no_inspector_assistance() {
                let table = [1; 32];
                let summary = InventorySummary {
                    count: 1,
                    fingerprint: [2; 32],
                };
                let classification = |report, table, assisted| {
                    super::classification(report, table, summary, assisted)
                };
                let report = Report {
                    table: Some(table),
                    local_known: Some(summary),
                    local_offered: Some(summary),
                    peer_known: Some(summary),
                    ..Report::default()
                };
                let positive = "preexisting-retention-without-inspector-repair";
                assert_eq!(classification(Some(report), &table, false), positive);
                assert_ne!(classification(Some(report), &table, true), positive);
                assert_ne!(classification(None, &table, false), positive);
                assert_ne!(
                    classification(
                        Some(Report {
                            conflicts_sent: 1,
                            ..report
                        }),
                        &table,
                        false
                    ),
                    positive
                );
                // Matching local/remote counts and IDs must still identify the
                // baseline that contained this run's requested marker IDs.
                let changed = InventorySummary {
                    count: 1,
                    fingerprint: [4; 32],
                };
                assert_ne!(
                    classification(
                        Some(Report {
                            local_known: Some(changed),
                            local_offered: Some(changed),
                            peer_known: Some(changed),
                            ..report
                        }),
                        &table,
                        false
                    ),
                    positive
                );
                assert_ne!(
                    classification(
                        Some(Report {
                            peer_known: None,
                            ..report
                        }),
                        &table,
                        false
                    ),
                    positive
                );
                assert_ne!(
                    classification(
                        Some(Report {
                            peer_known: Some(InventorySummary {
                                count: 1,
                                fingerprint: [3; 32]
                            }),
                            ..report
                        }),
                        &table,
                        false
                    ),
                    positive
                );
                let empty = InventorySummary {
                    count: 0,
                    fingerprint: [0; 32],
                };
                assert_ne!(
                    classification(
                        Some(Report {
                            local_known: Some(empty),
                            local_offered: Some(empty),
                            peer_known: Some(empty),
                            ..report
                        }),
                        &table,
                        false
                    ),
                    positive
                );
            }

            #[test]
            fn receives_and_key_handoffs_taint_current_and_later_inventory_evidence() {
                let table = [1; 32];
                let summary = InventorySummary {
                    count: 1,
                    fingerprint: [2; 32],
                };
                let equal = Report {
                    table: Some(table),
                    local_known: Some(summary),
                    local_offered: Some(summary),
                    peer_known: Some(summary),
                    ..Report::default()
                };
                let positive = "preexisting-retention-without-inspector-repair";
                let mut received_control = Effects::default();
                received_control.control(&ControlReport {
                    received: 1,
                    ..ControlReport::default()
                });
                let mut global = Effects::default();
                global.accumulate(&received_control);
                assert_ne!(
                    classification(Some(equal), &table, summary, global.assisted()),
                    positive
                );
                for changed in [
                    Report {
                        received: 1,
                        ..equal
                    },
                    Report {
                        keys_sent: 1,
                        ..equal
                    },
                    Report {
                        keys_received: 1,
                        ..equal
                    },
                    Report {
                        conflicts_received: 1,
                        ..equal
                    },
                ] {
                    assert_ne!(
                        classification(Some(changed), &table, summary, false),
                        positive,
                        "matching IDs do not erase this round's enabling effects"
                    );
                    let mut current = Effects::default();
                    current.application(&changed);
                    let mut later = Effects::default();
                    later.accumulate(&current);
                    assert_ne!(
                        classification(Some(equal), &table, summary, later.assisted()),
                        positive,
                        "a later zero-transfer round retains earlier receive/key assistance"
                    );
                }
                assert_eq!(
                    classification(Some(equal), &table, summary, Effects::default().assisted()),
                    positive
                );
                for cancelled in [false, true] {
                    let address = SocketAddr::from(([127, 0, 0, 1], 1));
                    let mut peers = BTreeMap::from([(
                        address,
                        Peer {
                            repair: None,
                            next: Instant::now(),
                            previous_unaccounted: false,
                        },
                    )]);
                    let mut global = Effects::default();
                    let mut rounds = Vec::new();
                    observe(
                        Finished {
                            address,
                            repair: Repair::default(),
                            exchange_started: false,
                            cancelled,
                            result: Ok(ControlReport::default()),
                        },
                        &table,
                        summary,
                        &mut peers,
                        &mut BTreeMap::new(),
                        &mut global,
                        &mut rounds,
                    );
                    assert_eq!(global.assisted(), cancelled);
                    assert_eq!(rounds[0]["cancelled"], json!(cancelled));
                    assert_eq!(
                        classification(Some(equal), &table, summary, global.assisted()) == positive,
                        !cancelled
                    );
                }
            }
        }
    }
}

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    match probe::run() {
        Ok(value) => {
            let proof = matches!(
                value["mode"].as_str(),
                Some("audit" | "serve" | "serve-live" | "receive-live")
            );
            let established = !proof
                || matches!(
                    value["outcome"].as_str(),
                    Some(
                        "preexisting-retention"
                            | "automatic-owner-retention"
                            | "automatic-receiver-retention"
                            | "retained-test-rows"
                    )
                );
            println!("{value}");
            if established {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::from(2)
            }
        }
        Err(error) => {
            eprintln!("commons_probe: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("commons_probe requires Unix Commons support");
    std::process::ExitCode::FAILURE
}
