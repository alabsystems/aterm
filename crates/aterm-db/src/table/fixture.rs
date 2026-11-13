// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A scratch Commons lab: `n` Macs, each its own vault custody under one temporary home that
//! goes when the lab drops, joined to one test vault and enrolled as the roles given, and the
//! repair exchange a runtime would run between them over loopback. The tests' fixture, and the
//! dev seam `lab reveal-tab` seeds its lab instance's world with (one-mesh design §11 A17); it
//! touches no real Mac's state. Absent from release builds.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "a scratch fixture: a step that fails leaves nothing to run, so it panics"
)]

use aterm_secrets::{
    client::{self, Paths},
    commons::{
        self,
        binding::Pending,
        bootstrap::{Bootstrap, Descriptor, Device},
        journal::Journal,
        roster::{Enrollment, Grant, Role},
    },
    text::Audience,
};
use std::{path::PathBuf, time::Duration};

/// The test vault's Commons network.
pub const NETWORK: [u8; 16] = [9; 16];

/// Signed table bytes with an explicit clock claim, without changing the host's clock.
#[cfg(test)]
pub(crate) fn seal_at(
    session: &super::Session<'_>,
    scope: super::Scope,
    causal: crate::crypto::Causal<'_>,
    payload: &[u8],
    authored_ms: u64,
) -> crate::Result<Vec<u8>> {
    session.seal_at(scope, causal, payload, authored_ms)
}

/// `rounds` repair passes in which the first Mac exchanges with every other, as a runtime does.
pub fn repair_commons(lab: &Lab, rounds: usize) {
    let members: Vec<usize> = (0..lab.paths.len()).collect();
    repair_commons_with(lab, rounds, &members, None, None, false);
}

/// [`repair_commons`] for a test whose setup requires every application round to complete.
/// A cut fails at that exchange, including its diagnostic, before any later state assertion.
#[cfg(test)]
pub fn repair_commons_complete(lab: &Lab, rounds: usize) {
    let members: Vec<usize> = (0..lab.paths.len()).collect();
    repair_commons_with(lab, rounds, &members, None, None, true);
}

/// [`repair_commons`] with each round repairing `table` first, as a push names it
/// (`crate::runtime::Pusher`), with no regular scan: for a test that knows which table moved. A
/// table the two sides of an exchange do not both hold falls back to one regular-scan exchange
/// while the side that lacks it takes it up, so each of the first Mac's exchanges must complete
/// and the last round must have repaired `table` with every peer: a test whose rounds fall short,
/// or whose exchange fails, fails here, not on what its repair did not deliver.
#[cfg(test)]
pub fn repair_commons_first(lab: &Lab, rounds: usize, table: [u8; 32]) {
    let members: Vec<usize> = (0..lab.paths.len()).collect();
    let reports = repair_commons_with(lab, rounds, &members, None, Some(table), true);
    assert_repaired(&reports[0], rounds, members.len() - 1, table);
}

/// [`repair_commons_among`] with each round repairing `table` first ([`repair_commons_first`]).
#[cfg(test)]
pub fn repair_commons_among_first(
    lab: &Lab,
    rounds: usize,
    members: &[usize],
    now: &[u64],
    table: [u8; 32],
) {
    assert_eq!(now.len(), lab.paths.len(), "one clock per Mac");
    let reports = repair_commons_with(lab, rounds, members, Some(now), Some(table), true);
    assert_repaired(&reports[0], rounds, members.len() - 1, table);
}

/// The first Mac's `reports` of `rounds` rounds with `peers` peers hold one per exchange, and the
/// last `peers`, the last round's, each repaired `table`. A failed exchange reports nothing, so
/// without the count the last `peers` would reach back into an earlier round.
#[cfg(test)]
fn assert_repaired(
    reports: &[crate::replica::Report],
    rounds: usize,
    peers: usize,
    table: [u8; 32],
) {
    assert_eq!(
        reports.len(),
        rounds * peers,
        "an exchange completed no application repair"
    );
    for report in &reports[reports.len() - peers..] {
        assert_eq!(
            report.table,
            Some(table),
            "the named table was not repaired in the last round"
        );
    }
}

/// [`repair_commons`] with each Mac's ring windows placed by its own clock, `now[n]` seconds.
#[cfg(test)]
pub fn repair_commons_at(lab: &Lab, rounds: usize, now: &[u64]) {
    let members: Vec<usize> = (0..lab.paths.len()).collect();
    repair_commons_among(lab, rounds, &members, now);
}

/// [`repair_commons_at`] among `members` alone (the first exchanges with each other), as one
/// side of a split.
#[cfg(test)]
pub fn repair_commons_among(lab: &Lab, rounds: usize, members: &[usize], now: &[u64]) {
    assert_eq!(now.len(), lab.paths.len(), "one clock per Mac");
    repair_commons_with(lab, rounds, members, Some(now), None, false);
}

/// [`repair_commons`], handing back each Mac's application report of every exchange, in order.
pub fn repair_commons_reports(lab: &Lab, rounds: usize) -> Vec<Vec<crate::replica::Report>> {
    let members: Vec<usize> = (0..lab.paths.len()).collect();
    repair_commons_with(lab, rounds, &members, None, None, false)
}

fn repair_commons_with(
    lab: &Lab,
    rounds: usize,
    members: &[usize],
    now: Option<&[u64]>,
    first: Option<[u8; 32]>,
    require_complete: bool,
) -> Vec<Vec<crate::replica::Report>> {
    use crate::control::{Endpoint, Repair, Side};
    use aterm_net::lan::{Connection, Scope as LinkScope};
    let mut endpoints: Vec<_> = members
        .iter()
        .map(|&n| {
            let endpoint = Endpoint::from_bootstrap(&lab.bootstrap(n))
                .unwrap()
                .unwrap()
                .with_commons(lab.paths[n].clone(), lab.paths[n].home.join("config.toml"))
                .unwrap();
            #[cfg(test)]
            if let Some(now) = now {
                return endpoint.with_ring_clock(now[n]);
            }
            #[cfg(not(test))]
            let _ = now;
            endpoint
        })
        .collect();
    if first.is_some() {
        // The first Mac initiates every exchange, and only an initiator reads what a push names.
        endpoints[0].prefer(std::sync::Arc::new(std::sync::Mutex::new(first)));
    }
    let mut repairs: Vec<_> = (0..endpoints.len()).map(|_| Repair::default()).collect();
    let mut reports = vec![Vec::new(); endpoints.len()];
    for _ in 0..rounds {
        for peer in 1..endpoints.len() {
            let (head, tail) = repairs.split_at_mut(peer);
            let left = &mut head[0];
            let right = &mut tail[0];
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();
            std::thread::scope(|tasks| {
                let incoming = tasks.spawn(|| {
                    endpoints[peer].exchange(
                        Connection::admit(
                            aterm_uds::accept(
                                &listener,
                                aterm_uds::AcceptWait::Forever,
                                aterm_uds::AcceptedMode::Blocking,
                            )
                            .unwrap()
                            .0,
                            LinkScope::Loopback,
                        )
                        .unwrap(),
                        Side::Responder,
                        right,
                    )
                });
                endpoints[0]
                    .exchange(
                        Connection::connect(address, LinkScope::Loopback, Duration::from_secs(60))
                            .unwrap(),
                        Side::Initiator,
                        left,
                    )
                    .unwrap();
                incoming.join().unwrap().unwrap();
            });
            if require_complete {
                for repair in [&*left, &*right] {
                    assert!(
                        repair.membership_error().is_none()
                            && repair.application_error().is_none()
                            && repair.application().is_some(),
                        "Commons application round did not complete: membership={:?}, application={:?}, phase={:?}",
                        repair.membership_error(),
                        repair.application_error(),
                        repair.phase()
                    );
                }
            }
            reports[0].extend(left.application());
            reports[peer].extend(right.application());
        }
    }
    reports
}

pub struct Lab {
    pub _home: aterm_tempfile::TempDir,
    pub paths: Vec<Paths>,
    pub signer: String,
    pub source: PathBuf,
}
impl Lab {
    pub fn new(roles: &[Role]) -> Self {
        Self::with_enrolled(roles, roles.len())
    }
    pub fn with_enrolled(roles: &[Role], enrolled: usize) -> Self {
        Self::build(roles, enrolled, false)
    }
    pub fn with_foundation(roles: &[Role]) -> Self {
        Self::build(roles, roles.len(), true)
    }
    /// A founded lab of `roles` whose Macs past the first `enrolled` joined the vault and never
    /// opened Commons: no device key, custody or control history.
    pub fn with_foundation_enrolled(roles: &[Role], enrolled: usize) -> Self {
        Self::build(roles, enrolled, true)
    }
    /// A lab whose home is `home` (Mac `n`'s at `<home>/m<n>`), enrolled as `roles`: a caller
    /// that must name the home, as `lab reveal-tab` names its lab instance's world.
    pub fn in_home(home: aterm_tempfile::TempDir, roles: &[Role]) -> Self {
        Self::build_in(home, roles, roles.len(), false)
    }
    fn build(roles: &[Role], enrolled: usize, foundation: bool) -> Self {
        #[cfg(test)]
        aterm_spec::fs_lane::join();
        Self::build_in(
            aterm_tempfile::tempdir().unwrap(),
            roles,
            enrolled,
            foundation,
        )
    }
    fn build_in(
        home: aterm_tempfile::TempDir,
        roles: &[Role],
        enrolled: usize,
        foundation: bool,
    ) -> Self {
        let source = home.path().join("test.vault");
        let paths: Vec<_> = (0..roles.len())
            .map(|n| Paths::for_home(&home.path().join(format!("m{}", n + 1))))
            .collect();
        let created =
            client::test_create_unbound(&paths[0], source.to_str().unwrap(), None, Some("m1"))
                .unwrap();
        let admin = client::admin_open(&paths[0]).unwrap();
        let mut document = admin.doc.clone();
        for (n, path) in paths.iter().enumerate().skip(1) {
            let identity = client::machine_identity(path).unwrap();
            document
                .add_machine(&format!("m{}", n + 1), &identity.recipient().to_bech32())
                .unwrap();
        }
        let hosts = || (1..=roles.len()).map(|n| format!("m{n}")).collect();
        let anchor = foundation
            .then(|| commons::node::Node::foundation(admin.commons_authority().unwrap()).unwrap());
        let network = anchor.as_ref().map_or(NETWORK, |node| node.network());
        let initial = anchor.map_or_else(
            || b"pending".to_vec(),
            |node| Descriptor::from_genesis(node).unwrap().bytes(),
        );
        document
            .set_entry(&Audience::Hosts(hosts()), commons::ENTRY, &initial)
            .unwrap();
        client::admin_publish(&paths[0], &admin, &document).unwrap();
        let admin = client::admin_open(&paths[0]).unwrap();
        let proofs: Vec<_> = paths
            .iter()
            .zip(roles)
            .enumerate()
            .take(enrolled)
            .map(|(n, (path, role))| {
                let device = Device::open(path).unwrap().unwrap();
                let request = device
                    .request(network, admin.commons_authority().unwrap())
                    .unwrap();
                let (mut pending, challenge) =
                    Pending::issue(&request, network, *request.binding().authority()).unwrap();
                let proof = pending
                    .verify(&device.answer(&request, &challenge).unwrap())
                    .unwrap();
                (format!("m{}", n + 1), proof, *role)
            })
            .collect();
        let descriptor = if foundation {
            let bootstrap = Bootstrap::cached(&paths[0]).unwrap().unwrap();
            let mut journal =
                Journal::open_existing(bootstrap.journal_dir(), bootstrap.trust()).unwrap();
            let grants = proofs
                .into_iter()
                .map(|(host, proof, role)| {
                    let certificate = admin
                        .approve_commons_member(&journal, &host, proof)
                        .unwrap();
                    Grant::certified(host, certificate, role)
                })
                .collect();
            let first = admin.prepare_commons_rotation(&journal, grants).unwrap();
            journal.ingest(first.bytes()).unwrap();
            Descriptor::from_journal(&journal).unwrap()
        } else {
            let enrollments = proofs
                .into_iter()
                .map(|(host, proof, role)| Enrollment::new(host, proof, role))
                .collect();
            Descriptor::from_genesis(admin.prepare_commons_genesis(network, enrollments).unwrap())
                .unwrap()
        };
        document
            .set_entry(
                &Audience::Hosts(hosts()),
                commons::ENTRY,
                &descriptor.bytes(),
            )
            .unwrap();
        client::admin_publish(&paths[0], &admin, &document).unwrap();
        for (n, path) in paths.iter().enumerate().skip(1) {
            client::join(
                path,
                &client::JoinOptions {
                    source: format!(
                        "{}#{}",
                        source.display(),
                        aterm_secrets::b64::encode_padded(&admin.config.signer.unwrap())
                    ),
                    host: Some(format!("m{}", n + 1)),
                    interactive: false,
                    wait: Duration::ZERO,
                    confirm: &|_| false,
                    password: aterm_secrets::client::JoinPassword::Given(None),
                    fetched: None,
                    requested: &|| {},
                    say: &|_| {},
                },
            )
            .unwrap();
        }
        Self {
            _home: home,
            paths,
            signer: created.signer,
            source,
        }
    }
    pub fn bootstrap(&self, n: usize) -> Bootstrap {
        Bootstrap::cached(&self.paths[n]).unwrap().unwrap()
    }
    /// Mac `n`'s machine, as its own Commons roster binds it.
    pub fn machine(&self, n: usize) -> [u8; 16] {
        let bootstrap = self.bootstrap(n);
        let session = super::Session::observe(&bootstrap).unwrap();
        *session
            .members()
            .unwrap()
            .iter()
            .find(|binding| binding.actor() == session.actor())
            .expect("a lab Mac is in its own roster")
            .machine()
    }
    pub fn admit(&self, roles: &[Role]) {
        let admin = client::admin_open(&self.paths[0]).unwrap();
        let bootstraps: Vec<_> = (0..self.paths.len()).map(|n| self.bootstrap(n)).collect();
        let mut journal =
            Journal::open_existing(bootstraps[0].journal_dir(), bootstraps[0].trust()).unwrap();
        let grants = roles
            .iter()
            .enumerate()
            .map(|(n, role)| {
                let device = Device::open(&self.paths[n]).unwrap().unwrap();
                let request = bootstraps[n].request().unwrap();
                let (mut pending, challenge) = Pending::issue(
                    &request,
                    *request.binding().network(),
                    *request.binding().authority(),
                )
                .unwrap();
                let possession = pending
                    .verify(&device.answer(&request, &challenge).unwrap())
                    .unwrap();
                let certificate = admin
                    .approve_commons_member(&journal, &format!("m{}", n + 1), possession)
                    .unwrap();
                Grant::certified(format!("m{}", n + 1), certificate, *role)
            })
            .collect();
        let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
        journal.ingest(node.bytes()).unwrap();
        drop(journal);
        for b in bootstraps.iter().skip(1) {
            Journal::open_existing(b.journal_dir(), b.trust())
                .unwrap()
                .ingest(node.bytes())
                .unwrap();
        }
    }
    /// A copy of this lab, its Macs and its vault, in a home of its own: for a walk that needs one
    /// built world per step, which a clone makes for next to nothing. Each Mac's `source.toml`
    /// names the copy's vault and its `state.toml` the source identity that path gives, so the
    /// copy publishes to and syncs from its own vault and shares no file with this lab or another
    /// copy; no file of it holds a path under this lab's home, the hex of a Mac's old source
    /// identity, an old custody identity or an old claimed directory. Each table's enrollment claim
    /// binds the copy of its directory, as the table's first open there would have. Equal Macs
    /// share the content-keyed process caches of `aterm-secrets`, which answer equal bytes alike
    /// (`tests::frontier::wide_lab` relies on the same).
    #[cfg(test)]
    pub fn fork(&self) -> Self {
        aterm_spec::fs_lane::join();
        let home = aterm_tempfile::tempdir().unwrap();
        // The clone takes the temporary folder's own name, so `_home` is the copy's home as it is
        // a built lab's.
        std::fs::remove_dir(home.path()).unwrap();
        copy_tree(self._home.path(), home.path());
        let (from, to) = (self._home.path(), home.path());
        let at = |path: &std::path::Path| to.join(path.strip_prefix(from).unwrap());
        let source = at(&self.source);
        let paths: Vec<_> = self
            .paths
            .iter()
            .map(|paths| Paths::for_home(&at(&paths.home)))
            .collect();
        let old: Vec<_> = paths
            .iter()
            .map(|paths| rebind(paths, from, to, &source))
            .collect();
        let mut needles = vec![format!("{}/", from.display())];
        needles.extend(old);
        let mut tables = std::collections::BTreeMap::new();
        directories(from, &mut |dir| {
            let identity = super::store::directory_identity(dir).unwrap();
            tables.insert(identity, dir.strip_prefix(from).unwrap().to_path_buf());
        });
        let copied = |claimed| {
            to.join(
                tables
                    .get(&claimed)
                    .expect("a claim's table directory is in the lab"),
            )
        };
        settle_copy(to, &copied, &needles);
        Self {
            _home: home,
            paths,
            signer: self.signer.clone(),
            source,
        }
    }
    pub fn rotate(&self, roles: &[Role]) {
        let admin = client::admin_open(&self.paths[0]).unwrap();
        let bootstraps: Vec<_> = (0..self.paths.len()).map(|n| self.bootstrap(n)).collect();
        let mut journal =
            Journal::open_existing(bootstraps[0].journal_dir(), bootstraps[0].trust()).unwrap();
        let grants = bootstraps
            .iter()
            .zip(roles)
            .enumerate()
            .map(|(n, (b, role))| {
                Grant::recorded(
                    format!("m{}", n + 1),
                    b.request().unwrap().binding().clone(),
                    *role,
                )
            })
            .collect();
        let node = admin.prepare_commons_rotation(&journal, grants).unwrap();
        journal.ingest(node.bytes()).unwrap();
        drop(journal);
        for b in bootstraps.iter().skip(1) {
            Journal::open_existing(b.journal_dir(), b.trust())
                .unwrap()
                .ingest(node.bytes())
                .unwrap();
        }
    }
}

/// Copy the tree at `from` to the new name `to`: a clone where the volume makes one, one call
/// whatever the tree holds; elsewhere each directory made again with its mode (as the umask
/// allows) and each regular file copied with its mode, any other kind of entry refused.
#[cfg(test)]
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    if aterm_tempfile::clone_tree(from, to).is_ok() {
        return;
    }
    let mode = std::fs::metadata(from).unwrap().permissions().mode();
    std::fs::DirBuilder::new()
        .mode(mode & 0o7777)
        .create(to)
        .unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            assert!(kind.is_file(), "a lab makes no link: {}", target.display());
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Point the copied Mac at `paths`, whose `source.toml` still names the vault under `from`, at
/// `source`, the copy's vault under `to`, and bind its `state.toml` to that source: the source
/// identity `aterm_secrets::client` keeps for the applied version, which this replaces where it
/// names the old source. Returns the old identity's hex, which no file of the copy may hold.
#[cfg(test)]
fn rebind(
    paths: &Paths,
    from: &std::path::Path,
    to: &std::path::Path,
    source: &std::path::Path,
) -> String {
    let before = client::config(paths).unwrap().unwrap();
    let source_file = paths.dir.join("source.toml");
    let text = std::fs::read_to_string(&source_file).unwrap();
    std::fs::write(
        &source_file,
        text.replace(
            &format!("{}/", from.display()),
            &format!("{}/", to.display()),
        ),
    )
    .unwrap();
    let after = client::config(paths).unwrap().unwrap();
    assert_eq!(
        after.source,
        aterm_secrets::transport::Source::Path(source.to_path_buf()),
        "the copy's Mac names the copy's vault"
    );
    let state_file = paths.dir.join("state.toml");
    let state = std::fs::read_to_string(&state_file).unwrap();
    let old = aterm_codec::hex::encode_lower(&client::test_source_identity(&before));
    assert_eq!(
        state.matches(&old).count(),
        1,
        "the Mac's state.toml binds its source identity once"
    );
    let new = aterm_codec::hex::encode_lower(&client::test_source_identity(&after));
    std::fs::write(&state_file, state.replace(&old, &new)).unwrap();
    old
}

/// `visit` each directory under `root`, `root` among them.
#[cfg(test)]
fn directories(root: &std::path::Path, visit: &mut impl FnMut(&std::path::Path)) {
    visit(root);
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            directories(&entry.path(), visit);
        }
    }
}

/// Settle `root`, a copy of a lab: bind each table claim to `copied` of the directory it claims,
/// put the custody identity each claim then has where its old one stood (a table's application
/// pins hold it), and check that no file holds any of `needles`, any old custody identity or
/// any old claimed directory.
#[cfg(test)]
fn settle_copy(root: &std::path::Path, copied: &dyn Fn([u8; 16]) -> PathBuf, needles: &[String]) {
    let mut files = Vec::new();
    files_under(root, &mut files);
    let mut needles: Vec<Vec<u8>> = needles
        .iter()
        .map(|needle| needle.clone().into_bytes())
        .collect();
    let mut moved = Vec::new();
    for path in &files {
        if path
            .parent()
            .and_then(std::path::Path::file_name)
            .is_none_or(|name| name != "table-claims")
        {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap();
        assert!(
            !name.ends_with(".volume"),
            "a lab's claims carry no volume witness: {name}"
        );
        if !name.ends_with(".claim") {
            continue;
        }
        let mut claim = std::fs::read(path).unwrap();
        let Some(claimed) = super::store::claimed_directory(&claim) else {
            continue;
        };
        let (old, new) = super::store::rebind_claim(&mut claim, &copied(claimed)).unwrap();
        std::fs::write(path, &claim).unwrap();
        needles.push(claimed.to_vec());
        for (old, new) in [
            (old.to_vec(), new.to_vec()),
            (
                aterm_codec::hex::encode_lower(&old).into_bytes(),
                aterm_codec::hex::encode_lower(&new).into_bytes(),
            ),
        ] {
            needles.push(old.clone());
            moved.push((old, new));
        }
    }
    for path in &files {
        let mut bytes = std::fs::read(path).unwrap();
        let mut changed = false;
        for (old, new) in &moved {
            while let Some(at) = find(&bytes, old) {
                bytes[at..at + old.len()].copy_from_slice(new);
                changed = true;
            }
        }
        if changed {
            std::fs::write(path, &bytes).unwrap();
        }
        for needle in &needles {
            assert!(
                find(&bytes, needle).is_none(),
                "{} names the lab it was copied from: {}",
                path.display(),
                String::from_utf8_lossy(needle)
            );
        }
    }
}

/// Each file under `root`, into `files`.
#[cfg(test)]
fn files_under(root: &std::path::Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            files_under(&entry.path(), files);
        } else {
            files.push(entry.path());
        }
    }
}

/// Where `needle` first stands in `bytes`.
#[cfg(test)]
fn find(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|window| window == needle)
}
