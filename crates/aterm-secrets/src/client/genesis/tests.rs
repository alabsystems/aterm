// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::commons::binding::{Pending, Request};
use crate::commons::journal::Journal;
use crate::membership_log::{Inserted, Log, MemberId};

const CASE: &str = "client::genesis::tests::every_current_member_is_in_after_genesis_v2";
const CLI_INPUT: &str = "ATERM_GENESIS_CLI_FIXTURE";

fn cli(home: &Path, args: &[&str], expected: u8, retired: bool) {
    use aterm_uds::spawnfd::Stdio;
    let tmp = home.join("tmp");
    fsx::ensure_private_dir(&tmp).unwrap();
    let mut command = aterm_uds::spawnfd::command(std::env::current_exe().unwrap());
    command
        .args(["--exact", CASE, "--test-threads=1", "--nocapture"])
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TMPDIR", &tmp)
        .env(
            CLI_INPUT,
            aterm_json::to_string(&aterm_json::json!({
                "args":args,"expected":expected,"retired":retired,
            }))
            .unwrap(),
        )
        .stdin(Stdio::null());
    let output = aterm_uds::deadline::output_within(&mut command, Duration::from_secs(60)).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && stdout
                .lines()
                .any(|line| line.ends_with("entered genesis CLI fixture")),
        "actual secrets CLI did not return {expected}: {}\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A scratch lab on a vault file: the admin `m22`, whose own key stands in for the ALab vault's.
struct Lab {
    root: aterm_tempfile::TempDir,
    paths: Paths,
    pinned: String,
    lineage: [u8; 32],
    server: Option<crate::server::Running>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop();
        }
    }
}

impl Lab {
    fn new() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m22"));
        let source = root.path().join("alab.vault");
        let created =
            test_create_unbound(&paths, source.to_str().unwrap(), None, Some("m22")).unwrap();
        // As a vault from before Commons custody: `admin_commons_init` founds its Commons.
        admin_keys::legacy::publish_legacy(&paths, &admin_open(&paths).unwrap());
        Self {
            pinned: format!("{}#{}", source.display(), created.signer),
            lineage: decode_key32(&created.signer).unwrap(),
            root,
            paths,
            server: None,
        }
    }

    fn served() -> Self {
        let root = aterm_tempfile::tempdir().unwrap();
        let paths = Paths::for_home(&root.path().join("m1"));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = crate::server::start(
            listener,
            &crate::server::Options {
                listen: address,
                store: root.path().join("served"),
                advertise: None,
                pins: Vec::new(),
                hold: Duration::from_millis(10),
                keep_awake: false,
            },
        )
        .unwrap();
        let mut lab = Self {
            root,
            paths,
            pinned: String::new(),
            lineage: [0; 32],
            server: Some(server),
        };
        let import = lab.root.path().join("import");
        fsx::ensure_private_dir(&import).unwrap();
        fsx::write_new(
            &import.join("seed-value"),
            b"private genesis fixture value",
            0o600,
        )
        .unwrap();
        let url = format!("http://{address}/v1/team");
        cli(
            &lab.paths.home,
            &[
                "create",
                &url,
                "--import",
                import.to_str().unwrap(),
                "--name",
                "m1",
                "--no-agent",
            ],
            0,
            false,
        );
        let cfg = config(&lab.paths).unwrap().unwrap();
        assert_eq!(cfg.source, Source::Url(url.clone()));
        lab.lineage = cfg.signer.unwrap();
        lab.pinned = format!("{url}#{}", crate::b64::encode_padded(&lab.lineage));
        lab
    }

    /// `host` joins with its own identity once the admin puts it in `[machines]`.
    fn join(&self, host: &str) -> Paths {
        let machine = Paths::for_home(&self.root.path().join(host));
        let recipient = machine_identity(&machine).unwrap().recipient().to_bech32();
        let admin = admin_open(&self.paths).unwrap();
        let mut doc = admin.doc.clone();
        doc.add_machine(host, &recipient).unwrap();
        admin_publish(&self.paths, &admin, &doc).unwrap();
        join(
            &machine,
            &JoinOptions {
                source: self.pinned.clone(),
                host: Some(host.into()),
                interactive: false,
                wait: Duration::ZERO,
                confirm: &|_| false,
                password: JoinPassword::Given(None),
                fetched: None,
                requested: &|| {},
                say: &|_| {},
            },
        )
        .unwrap();
        machine
    }

    /// The admin enrolls `machine` in Commons by the possession challenge: Commons then holds a
    /// binding for it.
    fn enroll(&self, machine: &Paths) {
        sync(machine, None).unwrap();
        let owner = Bootstrap::required(&self.paths).unwrap();
        let peer = Bootstrap::required(machine).unwrap();
        let request = peer.request().unwrap();
        let (mut pending, challenge) =
            Pending::issue(&request, owner.trust().network, owner.trust().authority).unwrap();
        let answer = peer.device().answer(&request, &challenge).unwrap();
        let mut journal = Journal::open_existing(owner.journal_dir(), owner.trust()).unwrap();
        admin_cached(&self.paths)
            .unwrap()
            .enroll_commons_member(&owner, &mut journal, pending.verify(&answer).unwrap())
            .unwrap();
    }

    fn recipients(&self) -> BTreeSet<Recipient> {
        admin_open(&self.paths)
            .unwrap()
            .doc
            .machines()
            .iter()
            .map(|machine| Recipient::parse(&machine.recipient).unwrap())
            .collect()
    }
}

#[test]
fn genesis_bindings_wait_for_the_local_writer() {
    let lab = Lab::new();
    admin_commons_init(&lab.paths).unwrap();
    let expected = Bootstrap::required(&lab.paths).unwrap().trust();
    let held = std::rc::Rc::new(std::cell::RefCell::new(Some(
        StateLock::take_at(&lab.paths, &lab.paths.lock_file()).unwrap(),
    )));
    let released = std::rc::Rc::clone(&held);
    let actual = crate::commons::bootstrap::tests::while_held(
        move || drop(released.borrow_mut().take()),
        || genesis_bootstrap_started(&lab.paths, Instant::now() - Duration::from_secs(31)),
    );
    drop(held.borrow_mut().take());
    assert_eq!(actual.unwrap().trust(), expected);

    let held = StateLock::take_at(&lab.paths, &lab.paths.lock_file()).unwrap();
    assert!(
        genesis_bootstrap_started(&lab.paths, Instant::now() - Duration::from_secs(61)).is_err()
    );
    drop(held);
    assert_eq!(
        genesis_bootstrap_started(&lab.paths, Instant::now())
            .unwrap()
            .trust(),
        expected
    );
}

#[test]
fn every_current_member_is_in_after_genesis_v2() {
    if let Ok(input) = std::env::var(CLI_INPUT) {
        let input: aterm_json::Value = aterm_json::from_str(&input).unwrap();
        let args = input["args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|word| std::ffi::OsString::from(word.as_str().unwrap()))
            .collect();
        if input["retired"].as_bool() == Some(true) {
            let cfg = config(&Paths::live().unwrap()).unwrap().unwrap();
            crate::envelope::TEST_RETIRED.with(|slot| {
                *slot.borrow_mut() = Some(Retired {
                    keys: vec![cfg.signer.unwrap()],
                    current: decode_key32(crate::ALAB_SIGNER),
                });
            });
        }
        println!("entered genesis CLI fixture");
        assert_eq!(
            crate::cli::main_entry(args),
            std::process::ExitCode::from(
                u8::try_from(input["expected"].as_u64().unwrap()).unwrap()
            )
        );
        return;
    }
    let lab = Lab::new();
    // m7 is in the vault when Commons is founded and enrolls; air joins later and holds no
    // binding.
    let m7 = lab.join("m7");
    admin_commons_init(&lab.paths).unwrap();
    let opened = admin_open(&lab.paths).unwrap();
    let bootstrap = Bootstrap::required(&lab.paths).unwrap();
    let before = bootstrap.access().unwrap().unwrap();
    lab.enroll(&m7);
    {
        let lock = StateLock::take_at(&lab.paths, &lab.paths.lock_file()).unwrap();
        let current = enrollment::admin_cached_held(&lab.paths, &lock).unwrap();
        assert_eq!(current.opened_heads, opened.opened_heads);
        let after = bootstrap.access_held(&lock).unwrap().unwrap();
        assert_ne!(after.head(), before.head());
        assert!(
            revalidate_private_genesis(&lab.paths, &opened, &bootstrap, &before, &lock).is_err(),
            "a Journal-only enrollment invalidates the captured founders before publication"
        );
        revalidate_private_genesis(&lab.paths, &current, &bootstrap, &after, &lock).unwrap();
    }
    let air = lab.join("air");
    let out = lab.root.path().join("alab.genesis");

    // Only a released aterm signs it, and a refusal writes nothing.
    assert!(sign_genesis(&lab.paths, &out, &lab.lineage, false).is_err());
    assert!(!out.exists());
    // Only the ALab vault's key signs it.
    assert!(sign_genesis(&lab.paths, &out, &[1; 32], true).is_err());
    assert!(!out.exists());

    let signed = seed_vault_genesis(&lab.paths, &out).unwrap();
    let record = Record::decode(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(*record.id(), signed.digest);
    assert_eq!(signed.members, 3);
    {
        let lock = StateLock::take_at(&lab.paths, &lab.paths.lock_file()).unwrap();
        assert!(
            enrollment::admin_cached_held(&lab.paths, &lock).is_ok(),
            "cached custody of a seeded file vault uses the caller's existing state lock"
        );
    }
    // Once: a second run refuses, and the file holds the first record.
    assert!(seed_vault_genesis(&lab.paths, &out).is_err());
    assert_eq!(std::fs::read(&out).unwrap(), record.bytes());

    // Every [machines] recipient is a founder: m22 and m7 with their bindings, air with none.
    let crate::membership_log::Body::Genesis { founders, .. } = record.body() else {
        panic!("not a genesis");
    };
    let listed: BTreeSet<Recipient> = founders.iter().map(|f| f.recipient).collect();
    assert_eq!(listed, lab.recipients());
    let air_recipient = machine_identity(&air).unwrap().recipient();
    for founder in founders {
        assert_eq!(
            founder.binding.is_some(),
            founder.recipient != air_recipient,
            "{}",
            founder.name
        );
    }

    // air proves its device key to m7, which lets it in: every current member is then in.
    let mut log = Log::new(record).unwrap();
    let member = Bootstrap::required(&m7).unwrap();
    let by: MemberId = member.request().unwrap().binding().actor();
    let trust = member.trust();
    let seed = [44; 32];
    let request = Request::create(
        trust.network,
        trust.authority,
        [44; 16],
        [45; 16],
        &machine_identity(&air).unwrap(),
        &seed,
    )
    .unwrap();
    let (proof, challenge) = log.challenge(&request, [46; 32], "air".into()).unwrap();
    let answer = request
        .answer(&machine_identity(&air).unwrap(), &seed, &challenge)
        .unwrap();
    let draft = proof.admit(&log, &answer, &by, 2).unwrap();
    let incarnation = *member.request().unwrap().binding().incarnation();
    let admit = member
        .device()
        .with_seed(|seed| draft.sign(seed, incarnation))
        .unwrap();
    assert_eq!(log.insert(admit).unwrap(), Inserted::Held);
    let membership = log.membership();
    let held: BTreeSet<Recipient> = membership
        .members()
        .values()
        .map(|m| m.binding.recipient())
        .collect();
    assert_eq!(held, lab.recipients());
    assert!(membership.unproved().is_empty());

    // The configured URL route is the actual CLI, ordinary create and approval; no file
    // configuration is converted into a URL profile and no fake signer returns a record.
    let served = Lab::served();
    let peer = Paths::for_home(&served.root.path().join("m2"));
    let recipient = machine_identity(&peer).unwrap().recipient().to_bech32();
    cli(
        &served.paths.home,
        &["approve", &recipient, "--name", "m2"],
        0,
        false,
    );
    cli(
        &peer.home,
        &[
            "join",
            &served.pinned,
            "--name",
            "m2",
            "--no-agent",
            "--no-wait",
        ],
        0,
        false,
    );
    let output = served.root.path().join("team.genesis");
    let args = ["genesis", output.to_str().unwrap()];
    cli(&served.paths.home, &args, 0, false);
    let bytes = std::fs::read(&output).unwrap();
    let record = Record::decode(&bytes).unwrap();
    let bootstrap = Bootstrap::required(&served.paths).unwrap();
    let crate::membership_log::Body::Genesis {
        signer,
        root,
        founders,
    } = record.body()
    else {
        panic!("the CLI wrote no genesis");
    };
    assert_eq!(signer, &served.lineage);
    assert_eq!(root.network, bootstrap.trust().network);
    assert_eq!(root.authority, bootstrap.trust().authority);
    assert_eq!(founders.len(), 2);
    assert_eq!(
        founders
            .iter()
            .map(|founder| founder.recipient)
            .collect::<BTreeSet<_>>(),
        served.recipients()
    );
    let saved = load_records(&served.paths).unwrap();
    let log = saved.log().unwrap();
    assert_eq!(log.genesis().id(), record.id());
    assert!(
        log.records()
            .any(|record| matches!(record.body(), crate::membership_log::Body::Epoch { .. }))
    );
    assert!(
        log.records()
            .any(|record| matches!(record.body(), crate::membership_log::Body::Put { .. }))
    );
    assert!(
        served
            .server
            .as_ref()
            .unwrap()
            .stored_records("team")
            .contains(record.id())
    );
    cli(&served.paths.home, &args, 1, false);
    assert_eq!(
        std::fs::read(&output).unwrap(),
        bytes,
        "a repeated CLI never replaces its first output"
    );

    let refused = served.root.path().join("refused.genesis");
    let args = ["genesis", refused.to_str().unwrap()];
    // A retired pin gives way and is not `alab_vault`; it still cannot use the private route.
    cli(&served.paths.home, &args, 1, true);
    assert!(!refused.exists());
    let cfg = config(&served.paths).unwrap().unwrap();
    let mut default = cfg.clone();
    default.source = Source::Url(crate::ALAB_DEFAULT_URL.into());
    save_config(&served.paths, &default).unwrap();
    cli(&served.paths.home, &args, 1, false);
    assert!(!refused.exists());
    save_config(&served.paths, &cfg).unwrap();
    // A verified cached handoff can carry a protected predecessor absent from followed=[].
    let linked = Paths::for_home(&served.root.path().join("linked"));
    let mut linked_cfg = cfg.clone();
    linked_cfg.followed.clear();
    save_config(&linked, &linked_cfg).unwrap();
    let source = std::fs::read_to_string(linked.source_file()).unwrap();
    fsx::write_atomic(
        &linked.source_file(),
        format!("{source}followed = []\n").as_bytes(),
        0o600,
    )
    .unwrap();
    let predecessor_seed = [53; 32];
    let predecessor = envelope::signer_public(&predecessor_seed).unwrap();
    let successor = admin_open(&served.paths).unwrap();
    let witness = envelope::sign_unchecked(
        &envelope::Unsigned {
            name: linked_cfg.name.clone(),
            seq: 1,
            time: 1,
            handoff: envelope::test_chain(&linked_cfg.name, &[predecessor_seed, successor.seed]),
            parents: Vec::new(),
            lockers: vec![envelope::Locker {
                label: "admin".into(),
                armored: envelope::test_armor(b"inert lineage witness"),
            }],
        },
        &successor.seed,
    )
    .unwrap();
    fsx::write_atomic(&linked.cache_file(), &witness, 0o600).unwrap();
    let linked_cfg = config(&linked).unwrap().unwrap();
    assert!(linked_cfg.followed.is_empty());
    assert!(
        Reads::new(&linked)
            .lineage(&linked_cfg)
            .contains(&predecessor)
    );
    assert!(private_realm(&linked, &linked_cfg).unwrap());
    crate::envelope::TEST_RETIRED.with(|slot| {
        let previous = slot.replace(Some(Retired {
            keys: vec![predecessor],
            current: decode_key32(crate::ALAB_SIGNER),
        }));
        assert!(
            !private_realm(&linked, &linked_cfg).unwrap(),
            "verified cached ALab ancestry retains the released-admin restriction"
        );
        slot.replace(previous);
    });
    let data = served.paths.records_data_file().unwrap();
    let held = std::fs::read(&data).unwrap();
    fsx::write_atomic(&data, b"malformed record stream", 0o600).unwrap();
    cli(&served.paths.home, &args, 1, false);
    assert!(
        !refused.exists(),
        "an unreadable stream is not a new private realm"
    );
    fsx::write_atomic(&data, &held, 0o600).unwrap();
    // A last record cut short is what this Mac's own writer leaves where an append died: the
    // file holds the records before it, read as strictly as ever.
    // NEGATIVE CONTROL (2026-11-11): read with `decode_records`, the cut tail refuses the
    // realm's own records (`the records before a cut`).
    assert!(private_realm(&served.paths, &cfg).unwrap());
    for cut in [1, 40, held.len() - 1] {
        fsx::write_atomic(&data, &[&held[..], &held[..cut]].concat(), 0o600).unwrap();
        assert!(
            private_realm(&served.paths, &cfg).expect("the records before a cut"),
            "cut at {cut}"
        );
    }
    let mut forged = held.clone();
    *forged.last_mut().unwrap() ^= 1;
    fsx::write_atomic(&data, &forged, 0o600).unwrap();
    assert!(
        private_realm(&served.paths, &cfg).is_err(),
        "a whole record that fails its signature is still refused"
    );
    fsx::write_atomic(&data, &held, 0o600).unwrap();
    let kept = data.with_extension("genesis-kept");
    std::fs::rename(&data, &kept).unwrap();
    std::os::unix::fs::symlink(&kept, &data).unwrap();
    cli(&served.paths.home, &args, 1, false);
    assert!(
        !refused.exists(),
        "an unsafe records path is not a new private realm"
    );
    std::fs::remove_file(&data).unwrap();
    std::fs::rename(&kept, &data).unwrap();
}
