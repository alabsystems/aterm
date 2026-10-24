// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`].

use super::*;
use aterm_net::fleet::{FleetEvent, Listening, read_key_file, serve};
use aterm_net::fleet_member::{Issuer, read_peers, serve_book, serve_join};
use aterm_uds::private_file::{remove_private, write_private};
use std::cell::{Cell, RefCell};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const HANG: Duration = Duration::from_secs(60);

struct World {
    _tmp: aterm_tempfile::TempDir,
    places: Places,
}

impl World {
    fn new(tag: &str) -> Self {
        let tmp = aterm_tempfile::Builder::new()
            .prefix(&format!("aterm-fleet-{tag}-"))
            .tempdir()
            .expect("a scratch directory");
        let places = Places {
            state: tmp.path().join("state"),
            config: tmp.path().join("cfg/aterm/aterm.toml"),
        };
        Self { _tmp: tmp, places }
    }

    fn config(&self) -> String {
        std::fs::read_to_string(&self.places.config).unwrap_or_default()
    }

    fn write_config(&self, text: &str) {
        std::fs::create_dir_all(self.places.config.parent().unwrap()).unwrap();
        std::fs::write(&self.places.config, text).unwrap();
    }

    fn lock_config(&self) -> std::path::PathBuf {
        let lock = self
            .places
            .config
            .with_file_name(".aterm.toml.aterm-write.lock");
        let _ = std::fs::remove_file(&lock);
        std::fs::create_dir(&lock).unwrap();
        lock
    }
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_owned()).collect()
}

fn usage_of(result: Result<Opts, Failure>) -> String {
    match result {
        Err(Failure::Usage(why)) => why,
        other => panic!("{other:?} should be a usage refusal"),
    }
}

fn a_code() -> String {
    JoinCode {
        secret: [7; member::SECRET_LEN],
        host: "m7.local".to_owned(),
        port: 7100,
    }
    .text()
}

#[test]
fn a_join_code_is_never_an_argument() {
    let code = a_code();
    let bare = code.strip_prefix(member::CODE_PREFIX).unwrap().to_owned();
    let upper = code.to_ascii_uppercase();
    let flagged = format!("--code={code}");
    let bare_flagged = format!("--code-file={bare}");
    for words in [
        vec![code.as_str()],
        vec![upper.as_str()],
        vec!["q7xk-29fa-pm3c-0000@m7.local:7100"],
        vec!["--code-file", "f", "extra"],
        vec![flagged.as_str()],
        vec![bare_flagged.as_str()],
        vec!["--allow", "content", code.as_str()],
        vec!["--code-file", code.as_str()],
        vec!["--code-file", bare.as_str()],
        vec!["--allow", code.as_str()],
        vec!["--port", bare.as_str()],
    ] {
        let why = usage_of(parse_join(&args(&words)));
        assert_eq!(why, NO_CODE_ARGUMENT, "{words:?}");
    }
    for verb in ["join", "init", "peers"] {
        assert_eq!(
            main_entry(verb, &[code.clone().into(), "--help".into()]),
            ExitCode::from(2),
            "`{verb} <code> --help` is refused, not answered with help"
        );
    }
    let opts = parse_join(&args(&[
        "--allow",
        "content",
        "--port=7101",
        "--code-file",
        "f",
    ]))
    .expect("join's own flags pass");
    assert_eq!(opts.allow, Some(FleetScope::Content));
    assert_eq!(opts.port, Some(7101));
    assert_eq!(opts.code_file.as_deref(), Some(Path::new("f")));
    assert!(matches!(
        parse_opts(&args(&["--host", "m7 local"]), &["--host"], false),
        Err(Failure::Usage(_))
    ));
}

#[test]
fn a_code_file_gives_its_first_line() {
    let world = World::new("codefile");
    let file = world.places.config.with_file_name("code");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "\n  atf1-abcd@m7:7100  \nsecond\n").unwrap();
    assert_eq!(
        read_code(Some(&file)).unwrap(),
        ("atf1-abcd@m7:7100".to_owned(), CodeFrom::File)
    );
    assert!(!CodeFrom::File.left_behind().is_empty());
    assert!(!CodeFrom::Piped.left_behind().is_empty());
    assert!(CodeFrom::Typed.left_behind().is_empty());
    std::fs::write(&file, "\n\n").unwrap();
    assert!(read_code(Some(&file)).is_err(), "an empty file is no code");
    let missing = file.with_file_name(a_code());
    match read_code(Some(&missing)) {
        Err(Failure::Failed(why)) => assert!(
            !why.contains(&*missing.to_string_lossy()) && !why.contains(&a_code()),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn fleet_keys_are_set_and_removed_in_place() {
    let set = |text: &str| {
        with_fleet(text, "0.0.0.0:7100", "content", false)
            .unwrap()
            .unwrap()
    };
    assert_eq!(
        fleet_table(&set("")).unwrap(),
        FleetTable {
            listen: Some("0.0.0.0:7100".to_owned()),
            allow: Some("content".to_owned()),
            peers: Vec::new(),
            drive_from: Vec::new(),
        }
    );
    let person = "# my settings\ntheme = \"Nord\" # dark\n\n[fleet]\n# the port\nlisten = \
                  \"0.0.0.0:7000\"\n\n[[fleet.peers]]\nhost = \"lab-mini:7101\"\n\n[harness]\n\
                  enabled = true\n";
    let edited = set(person);
    for kept in [
        "# my settings",
        "theme = \"Nord\" # dark",
        "# the port",
        "[harness]",
    ] {
        assert!(edited.contains(kept), "{kept:?} kept:\n{edited}");
    }
    let table = fleet_table(&edited).unwrap();
    assert_eq!(table.listen.as_deref(), Some("0.0.0.0:7100"));
    assert_eq!(table.peers, vec!["lab-mini:7101".to_owned()]);
    assert_eq!(
        edited.matches("[fleet]").count(),
        1,
        "edited where it stands:\n{edited}"
    );
    assert_eq!(
        with_fleet(&edited, "0.0.0.0:7100", "content", false),
        Ok(None)
    );

    for spelling in [
        "fleet.listen = \"0.0.0.0:7000\"\n",
        "fleet = { listen = \"0.0.0.0:7000\" }\n",
    ] {
        let table = fleet_table(&set(spelling)).unwrap();
        assert_eq!(table.allow.as_deref(), Some("content"), "{spelling}");
    }
    assert!(with_fleet("theme = \n", "0.0.0.0:7100", "facts", false).is_err());
    assert!(with_fleet("fleet = 5\n", "0.0.0.0:7100", "facts", false).is_err());

    let removed = without_fleet(&edited).unwrap().unwrap();
    let table = fleet_table(&removed).unwrap();
    assert_eq!((table.listen, table.allow), (None, None));
    assert_eq!(
        table.peers,
        vec!["lab-mini:7101".to_owned()],
        "a person's peers are kept"
    );
    assert!(removed.contains("# my settings") && removed.contains("[harness]"));
    let bare = without_fleet(&set("theme = \"Nord\"\n")).unwrap().unwrap();
    assert_eq!(bare.trim(), "theme = \"Nord\"", "an emptied [fleet] goes");
    assert_eq!(without_fleet("theme = \"Nord\"\n"), Ok(None));
}

struct Listener {
    running: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<()>>,
}

impl Listener {
    fn serve(listener: TcpListener, state: &Path) -> Self {
        Self::serving(
            listener,
            state,
            &[FleetOp::Survey, FleetOp::Book, FleetOp::Join],
        )
    }

    fn serving(listener: TcpListener, state: &Path, serves: &'static [FleetOp]) -> Self {
        Self::naming(listener, state, serves, None)
    }

    fn claiming(listener: TcpListener, state: &Path, claim: &str) -> Self {
        Self::naming(
            listener,
            state,
            &[FleetOp::Survey, FleetOp::Book, FleetOp::Join],
            Some(claim.to_owned()),
        )
    }

    fn naming(
        listener: TcpListener,
        state: &Path,
        serves: &'static [FleetOp],
        claim: Option<String>,
    ) -> Self {
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));
        let listening = Listening {
            secret: read_key_file(state).expect("init wrote the key"),
            scope: FleetScope::Facts,
            node: member::read_node_identity(state).expect("a node key it can read"),
            serves,
            slots: 8,
            per_source: 8,
            lifetime: HANG,
        };
        let issuer = Arc::new(Issuer {
            node: member::node_id(state).unwrap(),
            host: "127.0.0.1".to_owned(),
            port,
            version: "0.95.0".to_owned(),
        });
        let (state, secret) = (state.to_path_buf(), listening.secret.clone());
        let serving = std::thread::spawn({
            let running = Arc::clone(&running);
            move || {
                serve(
                    &listener,
                    &listening,
                    &running,
                    move |served, op| {
                        let proof = served.peer_node().cloned();
                        served.answer(|stream| {
                            stream.set_read_timeout(Some(HANG))?;
                            let from = stream.peer_addr()?.ip();
                            if op == FleetOp::Book {
                                let node = issuer.node.clone();
                                return serve_book(stream, &state, move || Ok(node)).map(|_| ());
                            }
                            if op != FleetOp::Join {
                                let mut byte = [0u8; 1];
                                while stream.read(&mut byte)? == 1 && byte[0] != b'\n' {}
                                let reply = format!(
                                    "OK 1\ninstance pid=1 host=127.0.0.1 version=0.95.0 \
                                     sessions=0 level=facts cut=0 at_ms=1 node={}\n",
                                    claim.as_deref().unwrap_or(&issuer.node)
                                );
                                return stream.write_all(reply.as_bytes());
                            }
                            let issuer = Issuer::clone(&issuer);
                            serve_join(
                                stream,
                                (from, proof.as_ref()),
                                (&state, &secret),
                                move || Ok(issuer),
                                1_760_000_000_000,
                            )
                            .map(|_| ())
                        })
                    },
                    |_: FleetEvent| {},
                );
            }
        });
        Self {
            running,
            serving: Some(serving),
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
    }
}

fn closed_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn said() -> String {
    SAID.with(|said| std::mem::take(&mut *said.borrow_mut()))
}

fn ok(result: Result<String, Failure>) -> String {
    match result {
        Ok(report) => report,
        Err(failure) => panic!("{failure:?}"),
    }
}

#[test]
fn init_then_join_registers_and_peers_checks_each_peer() {
    let issuer = World::new("issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let report = ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port, "--allow", "content"]),
    ));
    let code_path = issuer.places.state.join(CODE_FILE);
    let code = std::fs::read_to_string(&code_path).unwrap();
    assert!(
        report.contains(&code_path.display().to_string()),
        "{report}"
    );
    assert!(
        !report.contains(code.trim()),
        "the code is never printed:\n{report}"
    );
    let mode = std::fs::metadata(&code_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    let table = fleet_table(&issuer.config()).unwrap();
    assert_eq!(
        table.listen,
        Some(format!("127.0.0.1:{port}")),
        "a loopback fleet"
    );
    assert_eq!(table.allow.as_deref(), Some("content"));
    let key = read_key_file(&issuer.places.state).unwrap();
    ok(init(&issuer.places, &[]));
    for flags in [
        ["--port", "7400"],
        ["--host", "m8.local"],
        ["--allow", "facts"],
    ] {
        assert!(
            matches!(init(&issuer.places, &args(&flags)), Err(Failure::Usage(_))),
            "{flags:?} on an issuer takes --rotate"
        );
    }
    assert_eq!(read_key_file(&issuer.places.state).unwrap(), key);
    assert_eq!(std::fs::read_to_string(&code_path).unwrap(), code);
    assert_eq!(fleet_table(&issuer.config()).unwrap(), table);

    let _serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("member");
    let member_port = closed_port();
    let loopback = || Some("127.0.0.1".to_owned());
    let opts = Opts {
        port: Some(member_port),
        host: loopback(),
        ..Opts::default()
    };
    let joined = ok(join(&member.places, &opts, &code));
    assert!(
        joined.contains("allow      facts:")
            && joined.contains("`aterm fleet allow content` lets it"),
        "{joined}"
    );
    assert_eq!(
        read_key_file(&member.places.state).unwrap(),
        read_key_file(&issuer.places.state).unwrap(),
        "one secret on both"
    );
    assert_eq!(
        fleet_table(&member.config()).unwrap().listen,
        Some(format!("127.0.0.1:{member_port}"))
    );
    let member_node = member::node_id(&member.places.state).unwrap();
    let book = read_peers(&issuer.places.state).unwrap();
    assert_eq!(book.len(), 1);
    assert_eq!(book[0].node.as_deref(), Some(member_node.as_str()));
    assert_eq!(book[0].port, member_port);
    assert_eq!(book[0].via, Via::Joined);
    let key_of = |places: &Places| {
        member::read_node_identity(&places.state)
            .unwrap()
            .expect("init and join mint the node key")
            .key()
    };
    assert_eq!(book[0].key, Some(key_of(&member.places)));
    let theirs = read_peers(&member.places.state).unwrap();
    assert_eq!(theirs.len(), 1);
    assert_eq!(theirs[0].via, Via::Issuer);
    assert_eq!(theirs[0].key, Some(key_of(&issuer.places)));
    let again = Opts {
        host: loopback(),
        ..Opts::default()
    };
    ok(join(&member.places, &again, &code));
    assert_eq!(
        fleet_table(&member.config()).unwrap().listen,
        Some(format!("127.0.0.1:{member_port}")),
        "a join again keeps its port"
    );
    assert_eq!(
        read_peers(&issuer.places.state).unwrap()[0].port,
        member_port
    );
    let theirs = read_peers(&member.places.state).unwrap();

    let checked = |places: &Places| match check_peers(places, HANG) {
        Ok(listing) => listing.rows,
        Err(failure) => panic!("{failure:?}"),
    };
    let rows = checked(&issuer.places);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].peer.node.as_deref(), Some(member_node.as_str()));
    assert!(
        matches!(
            &rows[0].check,
            Some(Err(Unreached::Connect(addr, e)))
                if e.kind() == io::ErrorKind::ConnectionRefused && addr.port() == member_port
        ),
        "the member runs no aterm: {:?}",
        rows[0].check
    );
    let rows = checked(&member.places);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].peer.via, Via::Issuer);
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    assert!(
        matches!(
            &rows[0].check,
            Some(Ok(member::Checked { scope: FleetScope::Facts, node: Some(node), proven: true, .. }))
                if *node == issuer_node
        ),
        "the check names the node that answered, proven: {:?}",
        rows[0].check
    );
    let listed = ok(list_peers(&issuer.places, HANG));
    assert!(
        listed.contains(short(&member_node)) && listed.contains(&key_of(&member.places).short()),
        "the row names its node and the key it is bound to:\n{listed}"
    );

    let joined_key = read_key_file(&member.places.state).unwrap();
    for flags in [&[][..], &["--rotate"][..]] {
        assert!(
            matches!(
                init(&member.places, &args(flags)),
                Err(Failure::Failed(why)) if why.contains("aterm fleet leave")
            ),
            "a member's init {flags:?} is refused"
        );
    }
    assert_eq!(read_key_file(&member.places.state).unwrap(), joined_key);
    assert!(!member.places.state.join(CODE_FILE).exists());
    assert_eq!(read_peers(&member.places.state).unwrap(), theirs);

    let elsewhere = World::new("elsewhere");
    ok(init(
        &elsewhere.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let other_code = std::fs::read_to_string(elsewhere.places.state.join(CODE_FILE)).unwrap();
    let book = read_peers(&issuer.places.state).unwrap();
    let config = issuer.config();
    let key_path = issuer.places.state.join(KEY_FILE);
    for (offered, key_mode) in [(&code, 0o600), (&other_code, 0o600), (&code, 0o644)] {
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(key_mode)).unwrap();
        assert!(
            matches!(
                join(&issuer.places, &Opts::default(), offered),
                Err(Failure::Failed(_))
            ),
            "an issuer's join is refused (key {key_mode:o})"
        );
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read_to_string(&code_path).unwrap(), code);
        assert_eq!(read_key_file(&issuer.places.state).unwrap(), key);
        assert_eq!(read_peers(&issuer.places.state).unwrap(), book);
        assert_eq!(issuer.config(), config);
    }

    let other = World::new("typo");
    let at = code.find('-').unwrap() + 1;
    let wrong = if code.as_bytes()[at] == b'0' {
        "1"
    } else {
        "0"
    };
    let typo = format!("{}{wrong}{}", &code[..at], &code[at + 1..]);
    assert!(matches!(
        join(&other.places, &Opts::default(), &typo),
        Err(Failure::Failed(_))
    ));
    assert!(!other.places.state.exists(), "nothing written for a typo");

    let fresh = World::new("unanswered");
    assert!(matches!(
        join(&fresh.places, &Opts::default(), &other_code),
        Err(Failure::Failed(_))
    ));
    assert!(!fresh.places.state.join(KEY_FILE).exists(), "no key");
    assert!(!fresh.places.config.exists(), "no [fleet]");
    assert_eq!(read_peers(&fresh.places.state).unwrap(), Vec::new());
    let (held, config) = (
        read_key_file(&member.places.state).unwrap(),
        member.config(),
    );
    assert!(matches!(
        join(&member.places, &Opts::default(), &other_code),
        Err(Failure::Failed(_))
    ));
    assert_eq!(read_key_file(&member.places.state).unwrap(), held);
    assert_eq!(member.config(), config);
    assert_eq!(read_peers(&member.places.state).unwrap(), theirs);

    let kept = key_of(&member.places);
    let left = ok(leave(&member.places, &[]));
    assert!(left.contains("The issuer is not told"), "{left}");
    assert_eq!(key_of(&member.places), kept, "leave keeps the node key");
}

#[test]
fn a_machine_that_takes_a_members_node_id_is_refused() {
    let issuer = World::new("bind-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port]),
    ));
    let code = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    let _serving = Listener::serve(listener, &issuer.places.state);
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    let member = World::new("bind-member");
    ok(join(&member.places, &opts, &code));
    let node = member::node_id(&member.places.state).unwrap();
    let book = read_peers(&issuer.places.state).unwrap();

    let taker = World::new("bind-taker");
    member::ensure_private_dir(&taker.places.state).unwrap();
    write_private(
        &taker.places.state,
        member::NODE_FILE,
        format!("{node}\n").as_bytes(),
    )
    .unwrap();
    let refused = match join(&taker.places, &opts, &code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: a taker of a bound node is refused"),
    };
    assert!(
        refused.contains(&format!(
            "`aterm fleet peers rm {}` on the issuer",
            short(&node)
        )),
        "{refused}"
    );
    assert!(
        !taker.places.state.join(KEY_FILE).exists(),
        "no key written"
    );
    assert!(!taker.places.config.exists(), "no [fleet] written");
    assert_eq!(
        read_peers(&issuer.places.state).unwrap(),
        book,
        "the member's entry stands"
    );

    ok(peers(&issuer.places, &args(&["rm", short(&node)])));
    ok(join(&taker.places, &opts, &code));
    let rebound = read_peers(&issuer.places.state).unwrap();
    assert_eq!(
        rebound
            .iter()
            .map(|p| (p.node.clone(), p.key))
            .collect::<Vec<_>>(),
        vec![(
            Some(node),
            Some(
                member::read_node_identity(&taker.places.state)
                    .unwrap()
                    .unwrap()
                    .key()
            )
        )]
    );
}

#[test]
fn a_join_again_of_the_same_fleet_reaches_only_the_issuer_it_binds() {
    let issuer = World::new("bound-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port]),
    ));
    let code = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    let _serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("bound-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let book = read_peers(&member.places.state).unwrap();
    let (key, config) = (
        read_key_file(&member.places.state).unwrap(),
        member.config(),
    );

    let secret = JoinCode::parse(&code).unwrap().secret;
    let other = World::new("bound-other");
    let serve_other = || {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let at = JoinCode {
            secret,
            host: "127.0.0.1".to_owned(),
            port: listener.local_addr().unwrap().port(),
        };
        (Listener::serve(listener, &other.places.state), at.text())
    };
    ok(init(
        &other.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    member::hold_book(&other.places.state)
        .unwrap()
        .write_key(&secret)
        .unwrap();
    remove_private(&other.places.state, NODE_KEY_FILE).unwrap();
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    for proves in ["no node", "its own node"] {
        if proves == "its own node" {
            member::node_identity(&other.places.state).unwrap();
        }
        let (_serving_other, there) = serve_other();
        let refused = match join(&member.places, &opts, &there) {
            Err(Failure::Failed(why)) => why,
            result => panic!("{proves}: {result:?} should be refused"),
        };
        assert!(
            refused.contains(&format!(
                "for the issuer this Mac binds (node {})",
                short(&issuer_node)
            )) && refused.contains("`aterm fleet leave` here, then `aterm fleet join`"),
            "{proves}: {refused}"
        );
        assert_eq!(read_peers(&member.places.state).unwrap(), book, "{proves}");
        assert_eq!(read_key_file(&member.places.state).unwrap(), key);
        assert_eq!(member.config(), config);
        assert_eq!(
            read_peers(&other.places.state).unwrap(),
            Vec::new(),
            "{proves}: nothing was sent it"
        );
    }

    let issuer_of = |world: &World| {
        read_peers(&member.places.state)
            .unwrap()
            .into_iter()
            .filter(|peer| peer.via == Via::Issuer)
            .map(|peer| (peer.node, peer.key))
            .collect::<Vec<_>>()
            == [(
                Some(member::node_id(&world.places.state).unwrap()),
                member::read_node_identity(&world.places.state)
                    .unwrap()
                    .map(|me| me.key()),
            )]
    };
    ok(join(&member.places, &opts, &code));
    assert!(issuer_of(&issuer), "the bound issuer, reached again");
    ok(leave(&member.places, &[]));
    let (_serving_other, there) = serve_other();
    ok(join(&member.places, &opts, &there));
    assert!(issuer_of(&other), "a new issuer, after leave");
}

#[test]
fn a_join_again_reaches_only_the_issuer_it_binds_while_its_state_reads_as_open() {
    let (issuer, code, _serving) = issuing("open-issuer");
    let member = World::new("open-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    let (book, key, config) = (
        read_peers(&member.places.state).unwrap(),
        read_key_file(&member.places.state).unwrap(),
        member.config(),
    );

    let other = World::new("open-other");
    ok(init(
        &other.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let secret = JoinCode::parse(&code).unwrap().secret;
    member::hold_book(&other.places.state)
        .unwrap()
        .write_key(&secret)
        .unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let there = JoinCode {
        secret,
        host: "127.0.0.1".to_owned(),
        port: listener.local_addr().unwrap().port(),
    }
    .text();
    let _serving_other = Listener::serve(listener, &other.places.state);

    let state = member.places.state.clone();
    let key_file = state.join(KEY_FILE);
    let book_file = state.join(PEERS_FILE);
    let second = member.places.config.with_file_name("second-link");
    let set_mode = |path: &Path, bits| {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    };
    let open = |how: &str, open: bool| match (how, open) {
        ("directory", _) => set_mode(&state, if open { 0o755 } else { 0o700 }),
        ("key", _) => set_mode(&key_file, if open { 0o644 } else { 0o600 }),
        ("book", _) => set_mode(&book_file, if open { 0o644 } else { 0o600 }),
        (_, true) => std::fs::hard_link(&key_file, &second).unwrap(),
        (_, false) => std::fs::remove_file(&second).unwrap(),
    };
    let binds = format!(
        "for the issuer this Mac binds (node {})",
        short(&issuer_node)
    );
    for (how, says) in [
        ("directory", binds.as_str()),
        ("key", &binds),
        ("second link", &binds),
        ("book", "a join reads the book before it dials the issuer"),
    ] {
        open(how, true);
        let refused = match join(&member.places, &opts, &there) {
            Err(Failure::Failed(why)) => why,
            result => panic!("{how}: {result:?} should be refused"),
        };
        assert!(refused.contains(says), "{how}: {refused}");
        open(how, false);
        assert_eq!(read_peers(&member.places.state).unwrap(), book, "{how}");
        assert_eq!(read_key_file(&member.places.state).unwrap(), key, "{how}");
        assert_eq!(member.config(), config, "{how}");
        assert_eq!(
            read_peers(&other.places.state).unwrap(),
            Vec::new(),
            "{how}: nothing was sent it"
        );
    }

    open("directory", true);
    ok(join(&member.places, &opts, &code));
    assert_eq!(
        (issuer_bound(&member.places), member.config()),
        (Some(issuer_node), config),
        "the issuer it binds, reached again, and its grant kept"
    );

    let (third, third_code, _third_serving) = issuing("open-third");
    open("key", true);
    ok(join(&member.places, &opts, &third_code));
    assert_eq!(
        issuer_bound(&member.places),
        Some(member::node_id(&third.places.state).unwrap()),
        "the book binds no issuer of another fleet"
    );
}

#[test]
fn an_issuer_whose_state_reads_as_open_is_told_it_does_not_join_its_own_fleet() {
    let issuer = World::new("open-own");
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let state = &issuer.places.state;
    let code = std::fs::read_to_string(state.join(CODE_FILE)).unwrap();
    std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o755)).unwrap();
    let opts = Opts {
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    let refused = match join(&issuer.places, &opts, &code) {
        Err(Failure::Failed(why)) => why,
        result => panic!("{result:?} should be refused"),
    };
    assert!(
        refused.contains("it does not join its own fleet") && !refused.contains("leave"),
        "{refused}"
    );
    assert_eq!(
        std::fs::read_to_string(state.join(CODE_FILE)).unwrap(),
        code,
        "its fleet kept"
    );
}

#[test]
fn an_issuer_whose_code_cannot_be_read_is_never_told_to_leave() {
    let issuer = World::new("unread-own");
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let path = issuer.places.state.join(CODE_FILE);
    let code = std::fs::read_to_string(&path).unwrap();
    let opts = Opts {
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    let refused = |text: &str| match join(&issuer.places, &opts, text) {
        Err(Failure::Failed(why)) => why,
        result => panic!("{result:?} should be refused"),
    };
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let own = refused(&code);
    assert!(
        own.contains("it does not join its own fleet") && !own.contains("leave"),
        "{own}"
    );
    let other = JoinCode {
        secret: [7; member::SECRET_LEN],
        host: "127.0.0.1".to_owned(),
        port: closed_port(),
    };
    let other = refused(&other.text());
    assert!(
        other.contains("cannot be read") && !other.contains("leave"),
        "{other}"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::hard_link(&path, issuer.places.state.join("handed-out")).unwrap();
    let linked = refused(&code);
    assert!(
        linked.contains("it does not join its own fleet") && !linked.contains("leave"),
        "{linked}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        code,
        "its fleet kept"
    );
}

#[test]
fn a_leave_says_it_narrowed_an_open_state() {
    let world = World::new("leave-open");
    let state = &world.places.state;
    let open = || {
        std::fs::create_dir_all(state).unwrap();
        std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    let narrowed = format!(
        "{} was open to other users (755); it is 0700 now",
        state.display()
    );
    open();
    let none = ok(leave(&world.places, &[]));
    assert!(
        none.starts_with("aterm fleet leave: this machine is in no fleet\n")
            && none.contains(&narrowed),
        "{none}"
    );
    ok(init(
        &world.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    open();
    let left = ok(leave(&world.places, &[]));
    assert!(
        left.contains(&narrowed) && left.contains("This machine issued the fleet"),
        "{left}"
    );
}

#[test]
fn a_leave_where_no_writer_can_make_the_state_takes_the_fleet_out_of_aterm_toml() {
    let world = World::new("leave-unmade");
    let root = world.places.state.parent().unwrap().to_owned();
    std::fs::write(root.join("a-file"), b"x").unwrap();
    std::os::unix::fs::symlink(root.join("nowhere"), root.join("dangling")).unwrap();
    let unwritable = root.join("unwritable");
    std::fs::create_dir(&unwritable).unwrap();
    std::fs::set_permissions(&unwritable, std::fs::Permissions::from_mode(0o500)).unwrap();
    for odd in [
        "a-file",
        "a-file/state",
        "dangling/state",
        "unwritable/state",
    ] {
        let places = Places {
            state: root.join(odd),
            config: world.places.config.clone(),
        };
        world.write_config(&format!(
            "[fleet]\nlisten = \"0.0.0.0:{}\"\nallow = \"facts\"\n",
            closed_port()
        ));
        let listening = world.config();
        let refused = match init(&places, &racing_init()) {
            Err(Failure::Failed(why)) => why,
            other => panic!("{odd}: {other:?}"),
        };
        let shown = places.state.display().to_string();
        assert_eq!(refused.matches(&shown).count(), 1, "{odd}: {refused}");
        assert_eq!(
            world.config(),
            listening,
            "{odd}: an init there writes nothing"
        );
        let left = ok(leave(&places, &[]));
        assert!(
            left.contains("[fleet] listen, allow and drive_from removed"),
            "{odd}: {left}"
        );
        assert!(!places.state.is_dir(), "{odd}");
    }
    std::fs::set_permissions(&unwritable, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn a_leave_whose_state_it_makes_inherits_an_access_list_takes_the_fleet_out_of_aterm_toml() {
    let world = World::new("leave-acl");
    let parent = world.places.state.parent().unwrap().join("inherits");
    std::fs::create_dir(&parent).unwrap();
    let ran = aterm_uds::spawnfd::command("/bin/chmod")
        .args(["+a", "everyone allow list,search,directory_inherit"])
        .arg(&parent)
        .status()
        .expect("run chmod");
    assert!(ran.success(), "chmod +a");
    let places = Places {
        state: parent.join("state"),
        config: world.places.config.clone(),
    };
    world.write_config(&format!(
        "[fleet]\nlisten = \"0.0.0.0:{}\"\nallow = \"facts\"\n",
        closed_port()
    ));
    let listening = world.config();
    assert!(
        init(&places, &racing_init()).is_err(),
        "no writer can lock there"
    );
    assert_eq!(world.config(), listening, "an init there writes nothing");
    std::fs::remove_dir(&places.state).unwrap();
    let left = ok(leave(&places, &[]));
    assert!(
        left.contains("[fleet] listen, allow and drive_from removed"),
        "the first leave: {left}"
    );
    assert!(places.state.is_dir(), "made, as an init makes it");
}

#[test]
fn an_issuer_entry_an_older_aterm_wrote_binds_a_join_only_beside_a_key_held() {
    let (_first, first_code, _first_serving) = issuing("unmarked-first");
    let (other, other_code, _other_serving) = issuing("unmarked-other");
    let member = World::new("unmarked-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let mut text = String::from("# The fleet's peers, one per line.\n");
    for peer in read_as_older(&member.places.state) {
        text.push_str(&peer.line());
        text.push('\n');
    }
    write_private(&member.places.state, PEERS_FILE, text.as_bytes()).unwrap();
    let key_file = member.places.state.join(KEY_FILE);
    std::fs::set_permissions(&key_file, std::fs::Permissions::from_mode(0o644)).unwrap();

    let refused = match join(&member.places, &opts, &other_code) {
        Err(Failure::Failed(why)) => why,
        result => panic!("{result:?} should be refused"),
    };
    assert!(
        refused.contains("is not private")
            && refused.contains("cannot tell whether the code is the fleet")
            && !refused.contains("joined this fleet before"),
        "{refused}"
    );
    assert_eq!(
        read_peers(&other.places.state).unwrap(),
        Vec::new(),
        "nothing was sent it"
    );

    std::fs::remove_file(&key_file).unwrap();
    ok(join(&member.places, &opts, &other_code));
    assert_eq!(
        issuer_bound(&member.places),
        Some(member::node_id(&other.places.state).unwrap()),
        "no key held, so the old issuer binds nothing"
    );
}

#[test]
fn an_issuer_that_lost_its_node_key_is_bound_again_as_its_refusal_says() {
    let issuer = World::new("keyless-issuer");
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let secret =
        JoinCode::parse(&std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap())
            .unwrap()
            .secret;
    let serve = || {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let at = JoinCode {
            secret,
            host: "127.0.0.1".to_owned(),
            port: listener.local_addr().unwrap().port(),
        };
        (Listener::serve(listener, &issuer.places.state), at.text())
    };
    let member = World::new("keyless-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    let (serving, code) = serve();
    ok(join(&member.places, &opts, &code));
    drop(serving);
    let node = member::node_id(&issuer.places.state).unwrap();
    let book = read_peers(&member.places.state).unwrap();

    remove_private(&issuer.places.state, NODE_KEY_FILE).unwrap();
    let (serving, code) = serve();
    let refused = match join(&member.places, &opts, &code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: the bound issuer proves no node"),
    };
    assert!(
        refused.contains("`aterm fleet init` there")
            && refused.contains("`aterm fleet peers rm <its node>` here")
            && refused.contains("`aterm fleet leave` here"),
        "{refused}"
    );
    assert_eq!(read_peers(&member.places.state).unwrap(), book);
    drop(serving);

    ok(init(&issuer.places, &[]));
    let minted = member::read_node_identity(&issuer.places.state)
        .unwrap()
        .expect("init minted a node key")
        .key();
    let (_serving, code) = serve();
    let refused = match join(&member.places, &opts, &code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: the bound node under another key"),
    };
    assert!(
        refused.contains("`aterm fleet peers rm <its node>` here"),
        "{refused}"
    );
    ok(peers(&member.places, &args(&["rm", short(&node)])));
    ok(join(&member.places, &opts, &code));
    let issuer_here: Vec<_> = read_peers(&member.places.state)
        .unwrap()
        .into_iter()
        .filter(|peer| peer.via == Via::Issuer)
        .map(|peer| (peer.node, peer.key))
        .collect();
    assert_eq!(issuer_here, [(Some(node), Some(minted))]);
}

#[test]
fn a_join_whose_config_write_fails_after_the_issuer_recorded_it_finishes_on_a_join_again() {
    let issuer = World::new("recorded");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port]),
    ));
    let code = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    let _serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("half");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };

    member.write_config("fleet = 5\n");
    assert!(matches!(
        join(&member.places, &opts, &code),
        Err(Failure::Failed(_))
    ));
    assert_eq!(
        read_peers(&issuer.places.state).unwrap(),
        Vec::new(),
        "refused before the dial: not recorded"
    );
    assert!(!member.places.state.exists(), "nothing written here");

    member.write_config("theme = \"Nord\"\n");
    let lock = member
        .places
        .config
        .with_file_name(".aterm.toml.aterm-write.lock");
    std::fs::create_dir(&lock).unwrap();
    assert!(matches!(
        join(&member.places, &opts, &code),
        Err(Failure::Failed(_))
    ));
    assert_eq!(
        read_peers(&issuer.places.state).unwrap().len(),
        1,
        "the issuer recorded it"
    );
    assert_eq!(
        read_key_file(&member.places.state).unwrap(),
        read_key_file(&issuer.places.state).unwrap(),
        "the key is held"
    );
    assert_eq!(read_peers(&member.places.state).unwrap(), Vec::new());
    assert_eq!(
        fleet_table(&member.config()).unwrap().listen,
        None,
        "no [fleet] listen"
    );

    std::fs::remove_dir(&lock).unwrap();
    ok(join(&member.places, &opts, &code));
    assert_eq!(
        fleet_table(&member.config()).unwrap().listen,
        Some(format!("127.0.0.1:{}", opts.port.unwrap())),
        "a join again finishes it"
    );
    assert_eq!(
        read_peers(&member.places.state).unwrap()[0].via,
        Via::Issuer
    );
    assert_eq!(read_peers(&issuer.places.state).unwrap().len(), 1);
}

#[test]
fn rotate_mints_a_new_secret_and_keeps_the_roster() {
    let issuer = World::new("rotate");
    ok(init(&issuer.places, &args(&["--host", "m7.local"])));
    let before = read_key_file(&issuer.places.state).unwrap();
    let code_before = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    member::update_peers(&issuer.places.state, |book| {
        member::record(
            book,
            Peer {
                node: Some("ab".repeat(16)),
                key: None,
                key_from: None,
                host: "lab-mini".to_owned(),
                addr: None,
                port: 7100,
                version: None,
                seen_ms: None,
                via: Via::Joined,
                fleet: None,
            },
        )?;
        member::record(
            book,
            Peer {
                node: None,
                key: None,
                key_from: None,
                host: "by-hand".to_owned(),
                addr: None,
                port: 7100,
                version: None,
                seen_ms: None,
                via: Via::Manual,
                fleet: None,
            },
        )
    })
    .unwrap();
    let roster = read_peers(&issuer.places.state).unwrap();
    let report = ok(init(&issuer.places, &args(&["--rotate"])));
    assert_ne!(read_key_file(&issuer.places.state).unwrap(), before);
    assert_ne!(
        std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap(),
        code_before
    );
    assert_eq!(
        read_peers(&issuer.places.state).unwrap(),
        roster,
        "every member and every person's peer kept"
    );
    assert!(
        report.contains(&"ab".repeat(4)),
        "the member to join again is named:\n{report}"
    );
    assert_eq!(
        fleet_table(&issuer.config()).unwrap().listen.as_deref(),
        Some("0.0.0.0:7100"),
        "the port stays"
    );
    let code_of = |world: &World| {
        let text = std::fs::read_to_string(world.places.state.join(CODE_FILE)).unwrap();
        let code = JoinCode::parse(&text).unwrap();
        (code.host, code.port)
    };
    assert_eq!(
        code_of(&issuer),
        ("m7.local".to_owned(), 7100),
        "the host stays"
    );

    remove_private(&issuer.places.state, CODE_FILE).unwrap();
    assert!(matches!(init(&issuer.places, &[]), Err(Failure::Failed(_))));

    let looped = World::new("rotate-loopback");
    ok(init(
        &looped.places,
        &args(&["--host", "127.0.0.1", "--port", "17320"]),
    ));
    ok(init(&looped.places, &args(&["--rotate"])));
    assert_eq!(code_of(&looped), ("127.0.0.1".to_owned(), 17320));
    assert_eq!(
        fleet_table(&looped.config()).unwrap().listen.as_deref(),
        Some("127.0.0.1:17320")
    );
    ok(init(
        &looped.places,
        &args(&["--rotate", "--port", "17321"]),
    ));
    assert_eq!(code_of(&looped), ("127.0.0.1".to_owned(), 17321));

    let fresh = World::new("rotate-fresh");
    assert!(matches!(
        init(&fresh.places, &args(&["--rotate"])),
        Err(Failure::Usage(_))
    ));
    assert!(!fresh.places.state.join(KEY_FILE).exists());
}

#[test]
fn a_rotate_of_an_open_state_keeps_the_host_and_port_its_code_names() {
    let issuer = World::new("rotate-open");
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", "17322"]),
    ));
    let state = &issuer.places.state;
    let before = read_key_file(state).unwrap();
    std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o755)).unwrap();
    let report = ok(init(&issuer.places, &args(&["--rotate"])));
    assert!(
        report.contains(&format!(
            "{} was open to other users (755); it is 0700 now",
            state.display()
        )),
        "{report}"
    );
    assert_ne!(read_key_file(state).unwrap(), before);
    let code = JoinCode::parse(&std::fs::read_to_string(state.join(CODE_FILE)).unwrap()).unwrap();
    assert_eq!((code.host, code.port), ("127.0.0.1".to_owned(), 17322));
}

#[test]
fn an_issuers_init_again_narrows_an_open_state_and_says_so() {
    let issuer = World::new("init-open");
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let state = &issuer.places.state;
    let code = std::fs::read_to_string(state.join(CODE_FILE)).unwrap();
    let key = read_key_file(state).unwrap();
    // 644: its owner cannot look a file up in it until it is narrowed.
    for mode in [0o755, 0o644] {
        std::fs::set_permissions(state, std::fs::Permissions::from_mode(mode)).unwrap();
        let again = ok(init(&issuer.places, &[]));
        assert!(
            again.contains("already the fleet's issuer")
                && again.contains(&format!(
                    "{} was open to other users ({mode:o}); it is 0700 now",
                    state.display()
                )),
            "{again}"
        );
        assert_eq!(
            std::fs::metadata(state).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            (
                std::fs::read_to_string(state.join(CODE_FILE)).unwrap(),
                read_key_file(state).unwrap()
            ),
            (code.clone(), key.clone()),
            "{mode:o}: its fleet kept"
        );
    }
}

#[test]
fn a_members_init_of_a_state_its_owner_cannot_search_is_refused() {
    let (_issuer, code, _serving) = issuing("init-unsearched-issuer");
    let member = World::new("init-unsearched-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let state = &member.places.state;
    let key = read_key_file(state).unwrap();
    std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o644)).unwrap();
    let refused = init(
        &member.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    );
    assert!(
        matches!(&refused, Err(Failure::Failed(why)) if why.contains("a member of a fleet it joined")),
        "{refused:?}"
    );
    assert!(said().contains("was open to other users (644)"));
    assert_eq!(read_key_file(state).unwrap(), key, "its fleet kept");
    assert!(!state.join(CODE_FILE).exists());
}

#[test]
fn a_verb_the_books_lock_refuses_still_says_it_narrowed_an_open_state() {
    let issuer = World::new("held-open-issuer");
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let member = World::new("held-open-member");
    std::fs::create_dir_all(&member.places.state).unwrap();
    std::fs::set_permissions(&member.places.state, std::fs::Permissions::from_mode(0o700)).unwrap();
    let opts = Opts {
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    let code = a_code();
    for issues in [true, false] {
        let state = if issues {
            &issuer.places.state
        } else {
            &member.places.state
        };
        let _held = member::hold_book(state).unwrap();
        std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = if issues {
            init(&issuer.places, &[])
        } else {
            join(&member.places, &opts, &code)
        };
        assert!(
            matches!(&refused, Err(Failure::Failed(why)) if why.contains("writing the book")),
            "{refused:?}"
        );
        assert!(
            said().contains(&format!(
                "{} was open to other users (755); it is 0700 now",
                state.display()
            )),
            "{refused:?}"
        );
    }
}

#[test]
fn every_verb_that_reads_the_state_narrows_an_open_one_and_says_so() {
    let (issuer, code, _serving) = issuing("verbs-open-issuer");
    let member = World::new("verbs-open-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let state = &member.places.state;
    let narrowed = format!(
        "{} was open to other users (755); it is 0700 now",
        state.display()
    );
    for verb in [
        vec!["peers"],
        vec!["allow", "drive", "--from", &issuer_node],
        vec!["peers", "rm", &issuer_node],
    ] {
        std::fs::set_permissions(state, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rest = args(&verb[1..]);
        let report = ok(if verb[0] == "peers" {
            peers(&member.places, &rest)
        } else {
            allow(&member.places, &rest)
        });
        assert!(report.contains(&narrowed), "{verb:?}: {report}");
        assert_eq!(
            std::fs::metadata(state).unwrap().permissions().mode() & 0o777,
            0o700,
            "{verb:?}"
        );
    }
}

#[test]
fn a_machine_the_book_holds_is_read_once() {
    let world = World::new("once");
    world.write_config(
        "[[fleet.peers]]\nhost = \"lab-mini:7100\"\n[[fleet.peers]]\nhost = \"10.0.0.5\"\n\
         [[fleet.peers]]\nhost = \"lab-mini:7101\"\n[[fleet.peers]]\nhost = \"lab-maxi\"\n",
    );
    member::ensure_private_dir(&world.places.state).unwrap();
    let joined = Peer {
        node: Some("ab".repeat(16)),
        key: None,
        key_from: None,
        host: "lab-mini.local".to_owned(),
        addr: Some(IpAddr::from([10, 0, 0, 5])),
        port: 7100,
        version: None,
        seen_ms: None,
        via: Via::Joined,
        fleet: None,
    };
    member::update_peers(&world.places.state, |book| {
        member::record(book, joined.clone())
    })
    .unwrap();
    let known = match known_peers(&world.places) {
        Ok(known) => known,
        Err(failure) => panic!("{failure:?}"),
    };
    let targets: Vec<String> = known.peers.iter().map(|(peer, _)| peer.target()).collect();
    assert_eq!(
        targets,
        ["10.0.0.5:7100", "lab-mini:7101", "lab-maxi:7100"],
        "{targets:?}"
    );

    let answered = |node: Option<&str>| {
        Ok(member::Surveyed {
            scope: FleetScope::Facts,
            addr: "10.0.0.5:7100".parse().unwrap(),
            node: node.map(str::to_owned),
            proven: false,
            key: None,
            lines: Vec::new(),
        })
    };
    let entry = |node: Option<&str>| Peer {
        node: node.map(str::to_owned),
        ..manual_peer("lab-mini".to_owned(), 7100)
    };
    let (a, b) = ("aa".repeat(16), "bb".repeat(16));
    let read = once_per_node(
        &[
            entry(None),
            entry(None),
            entry(Some(&a)),
            entry(None),
            entry(None),
            entry(None),
            entry(None),
        ],
        vec![
            answered(Some(&a)),
            answered(Some(&b)),
            answered(Some(&a)),
            Err(Unreached::Resolve("lab-maxi".to_owned())),
            answered(Some(&b)),
            answered(None),
            answered(None),
        ],
    );
    let kept: Vec<bool> = read.iter().map(Option::is_some).collect();
    assert_eq!(kept, [false, true, true, true, false, true, true]);
}

#[test]
fn an_unproven_claim_never_collapses_into_a_proven_answer() {
    let answered = |node: &str, proven: bool| {
        Ok(member::Surveyed {
            scope: FleetScope::Facts,
            addr: "10.0.0.5:7100".parse().unwrap(),
            node: Some(node.to_owned()),
            proven,
            key: None,
            lines: Vec::new(),
        })
    };
    let entry = |node: Option<&str>| Peer {
        node: node.map(str::to_owned),
        ..manual_peer("lab-mini".to_owned(), 7100)
    };
    let (mine, a) = ("aa".repeat(16), "bb".repeat(16));
    let read = once_per_node(
        &[
            entry(None),
            entry(None),
            entry(None),
            entry(None),
            entry(Some(&a)),
            entry(None),
        ],
        vec![
            answered(&mine, true),
            answered(&mine, false),
            answered(&mine, false),
            answered(&mine, true),
            answered(&a, true),
            answered(&a, false),
        ],
    );
    let kept: Vec<bool> = read.iter().map(Option::is_some).collect();
    assert_eq!(kept, [true, true, false, false, true, true]);
}

#[test]
fn a_mac_that_did_not_answer_is_one_row_at_its_place() {
    let by_hand = |port: u16| manual_peer("lab-mini".to_owned(), port);
    let joined_as = |node: &str, host: &str, addr: [u8; 4]| Peer {
        node: Some(node.to_owned()),
        addr: Some(IpAddr::from(addr)),
        via: Via::IssuerBook,
        ..manual_peer(host.to_owned(), 7100)
    };
    let joined = |node: &str| joined_as(node, "lab-mini.local", [10, 0, 0, 5]);
    let failed = |at: &str, kind: io::ErrorKind| {
        Err(Unreached::Connect(
            at.parse().unwrap(),
            io::Error::from(kind),
        ))
    };
    let down = || failed("10.0.0.5:7100", io::ErrorKind::TimedOut);
    let up = |node: &str| {
        Ok(member::Surveyed {
            scope: FleetScope::Facts,
            addr: "10.0.0.5:7100".parse().unwrap(),
            node: Some(node.to_owned()),
            proven: true,
            key: None,
            lines: Vec::new(),
        })
    };
    let kept = |entries: &[Peer], read: Vec<Result<member::Surveyed, Unreached>>| {
        once_per_node(entries, read)
            .iter()
            .map(Option::is_some)
            .collect::<Vec<_>>()
    };
    let (a, b) = ("aa".repeat(16), "bb".repeat(16));
    assert_eq!(
        kept(&[by_hand(7100), joined(&a)], vec![down(), down()]),
        [false, true],
        "the Mac asleep, once"
    );
    assert_eq!(
        kept(&[by_hand(7100), joined(&a)], vec![down(), up(&a)]),
        [true, true],
        "an answer hides no failure"
    );
    assert_eq!(
        kept(
            &[joined(&a), joined(&b), by_hand(7100)],
            vec![down(), down(), down()]
        ),
        [true, true, false],
        "two nodes at one place, two rows"
    );
    assert_eq!(
        kept(&[by_hand(7101), joined(&a)], vec![down(), down()]),
        [true, true],
        "another port"
    );
    let claimed = joined_as(&a, "lab-mini.evil", [10, 0, 0, 66]);
    assert_eq!(
        kept(
            &[by_hand(7100), claimed.clone()],
            vec![down(), failed("10.0.0.66:7100", io::ErrorKind::TimedOut)]
        ),
        [true, true],
        "a place its registrant claimed, failing at its own address"
    );
    assert_eq!(
        kept(
            &[by_hand(7100), claimed],
            vec![
                Err(Unreached::Resolve("lab-mini".to_owned())),
                Err(Unreached::Deadline(
                    member::Stage::Handshake,
                    Duration::from_secs(3)
                )),
            ]
        ),
        [true, true],
        "a failure at no address"
    );
    assert_eq!(
        kept(
            &[by_hand(7100), joined(&a)],
            vec![
                failed("10.0.0.5:7100", io::ErrorKind::ConnectionRefused),
                down()
            ]
        ),
        [true, true],
        "another failure at the same address"
    );
}

#[test]
fn a_members_peers_reads_its_issuers_book_and_keeps_a_copy() {
    let issuer = World::new("book-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("book-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    let code = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    ok(join(&member.places, &opts, &code));
    let named = closed_port();
    let named_at = format!("127.0.0.1:{named}");
    ok(peers(&issuer.places, &args(&["add", &named_at])));

    let listing = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    let rows: Vec<(Via, String)> = listing
        .rows
        .iter()
        .map(|row| (row.peer.via, row.peer.target()))
        .collect();
    assert_eq!(
        rows,
        [
            (Via::Issuer, format!("127.0.0.1:{port}")),
            (Via::IssuerBook, named_at.clone()),
        ],
        "the issuer, then its book's peer; never this machine's own registration"
    );
    assert!(
        matches!(&listing.rows[1].check, Some(Err(Unreached::Connect(_, e))) if e.kind() == io::ErrorKind::ConnectionRefused),
        "{:?}",
        listing.rows[1].check
    );
    assert_eq!(listing.note, None);
    let whole = aterm_ctl::FleetAsk {
        host: None,
        within: HANG,
    };
    let read = survey_fleet_in(&member.places, &whole).unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(read.peers.len(), 2, "{:?}", read.peers);
    assert_eq!(read.note, None, "a book that came leaves no line");
    let copies = || -> Vec<String> {
        member::read_issuer_book(&member.places.state, &issuer_node)
            .into_iter()
            .map(|peer| peer.target())
            .collect()
    };
    assert_eq!(copies(), [named_at.as_str()], "the member keeps the copy");
    let vias: Vec<Via> = read_peers(&member.places.state)
        .unwrap()
        .into_iter()
        .map(|peer| peer.via)
        .collect();
    assert_eq!(vias, [Via::Issuer], "the book holds no copy");
    assert!(ok(list_peers(&member.places, HANG)).contains("issuer-book"));

    assert!(matches!(
        peers(&member.places, &args(&["rm", &named_at])),
        Err(Failure::Failed(_))
    ));
    assert_eq!(
        copies(),
        [named_at.as_str()],
        "a copy is not taken out by hand"
    );

    ok(peers(&issuer.places, &args(&["rm", &named_at])));
    let listing = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(listing.rows.len(), 1, "{:?}", listing.rows.len());
    assert_eq!(
        copies(),
        Vec::<String>::new(),
        "the copy follows the issuer's book"
    );

    ok(peers(&issuer.places, &args(&["add", &named_at])));
    let _ = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(copies(), [named_at.as_str()]);
    ok(peers(
        &member.places,
        &args(&["rm", &format!("127.0.0.1:{port}")]),
    ));
    let copy_file = member.places.state.join(ISSUER_BOOK_FILE);
    assert!(!copy_file.exists(), "the copy went with the issuer");
    let stale: Vec<Peer> = vec![Peer {
        via: Via::IssuerBook,
        ..manual_peer("127.0.0.1".to_owned(), named)
    }];
    member::hold_book(&member.places.state)
        .unwrap()
        .keep_issuer_book(Some((&issuer_node, &stale)))
        .unwrap();
    let listing = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(listing.rows.len(), 0, "an orphan copy is never read");
    assert!(!copy_file.exists(), "an orphan copy is cleared");
    ok(join(&member.places, &opts, &code));

    let _ = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    assert!(copy_file.exists(), "a read kept a copy");
    let left = ok(leave(&member.places, &[]));
    assert!(!copy_file.exists(), "leave removes the copy");
    assert!(
        left.lines()
            .any(|line| line.contains(&copy_file.display().to_string())),
        "{left}"
    );
    ok(join(&member.places, &opts, &code));

    let _ = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(copies(), [named_at.as_str()]);
    drop(serving);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let older_port = listener.local_addr().unwrap().port();
    let _older = Listener::serving(
        listener,
        &issuer.places.state,
        &[FleetOp::Survey, FleetOp::Join],
    );
    member::update_peers(&member.places.state, |book| {
        for peer in book.iter_mut().filter(|peer| peer.via == Via::Issuer) {
            peer.port = older_port;
        }
        Ok(())
    })
    .unwrap();
    let listing = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(listing.rows.len(), 1);
    assert!(
        listing.note.is_some(),
        "the read says it is short of the book"
    );
    assert_eq!(copies(), Vec::<String>::new());
    let read = survey_fleet_in(&member.places, &whole).unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(read.peers.len(), 1, "{:?}", read.peers);
    assert_eq!(
        read.note, listing.note,
        "the survey's line is the listing's"
    );
}

#[test]
fn a_proven_mac_that_names_this_one_in_its_survey_is_refused_not_hidden() {
    let issuer = World::new("misnamed-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let honest = Listener::serve(listener, &issuer.places.state);
    let member = World::new("misnamed-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    let code = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    ok(join(&member.places, &opts, &code));
    let whole = aterm_ctl::FleetAsk {
        host: None,
        within: HANG,
    };
    let only = || {
        let mut read =
            survey_fleet_in(&member.places, &whole).unwrap_or_else(|why| panic!("{why}"));
        assert_eq!(read.peers.len(), 1, "{:?}", read.peers);
        read.peers.remove(0)
    };
    let read = only();
    assert!(read.answer.is_ok() && !read.unproven, "{read:?}");
    drop(honest);

    let mine = member::node_id(&member.places.state).unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let claiming_port = listener.local_addr().unwrap().port();
    let _claiming = Listener::claiming(listener, &issuer.places.state, &mine);
    member::update_peers(&member.places.state, |book| {
        for peer in book.iter_mut().filter(|peer| peer.via == Via::Issuer) {
            peer.port = claiming_port;
        }
        Ok(())
    })
    .unwrap();
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let read = only();
    let why = read.answer.as_ref().err().cloned().unwrap_or_default();
    assert!(
        why.contains(&format!(
            "proved node {}, its survey says node {}: not read",
            &issuer_node[..8],
            &mine[..8]
        )),
        "{read:?}"
    );
    assert!(!read.unproven, "refused, not read as a claim");
}

#[test]
fn a_read_short_of_its_issuers_book_says_whether_it_held_a_copy() {
    let issuer = Peer {
        node: Some("11".repeat(16)),
        key: None,
        via: Via::Issuer,
        ..manual_peer("m7.local".to_owned(), member::DEFAULT_PORT)
    };
    let copy = Peer {
        via: Via::IssuerBook,
        ..manual_peer("lab-mini.local".to_owned(), member::DEFAULT_PORT)
    };
    let unshared = BookRead::Unshared("not-now".to_owned());
    assert_eq!(
        book_short(std::slice::from_ref(&issuer), &unshared),
        Some(Short::Unshared {
            why: "not-now",
            copies: false
        })
    );
    assert_eq!(
        book_short(&[issuer.clone(), copy], &unshared),
        Some(Short::Unshared {
            why: "not-now",
            copies: true
        })
    );
    let at: std::net::SocketAddr = (Ipv4Addr::LOCALHOST, member::DEFAULT_PORT).into();
    let older = BookRead::Refused(BookUnread::Older(at));
    assert_eq!(
        book_short(std::slice::from_ref(&issuer), &older),
        Some(Short::Older)
    );
    let unread = BookRead::Unread("no connection".to_owned());
    assert_eq!(book_short(std::slice::from_ref(&issuer), &unread), None);
    assert_eq!(book_short(&[], &BookRead::NoIssuer), None);
}

#[test]
fn a_configured_peer_stands_for_a_copy_at_its_place() {
    let issuer = World::new("cfg-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let _serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("cfg-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    let code = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    ok(join(&member.places, &opts, &code));
    let named_at = format!("127.0.0.1:{}", closed_port());
    ok(peers(&issuer.places, &args(&["add", &named_at])));
    member.write_config(&format!(
        "{}\n[[fleet.peers]]\nhost = \"{named_at}\"\n",
        member.config()
    ));
    let named = |listing: &Listing| -> Vec<(Via, bool)> {
        listing
            .rows
            .iter()
            .filter(|row| row.peer.target() == named_at)
            .map(|row| (row.peer.via, row.configured))
            .collect()
    };
    for read in ["first", "second, with the copy kept"] {
        let listing = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
        assert_eq!(
            named(&listing),
            [(Via::Manual, true)],
            "{read} read: the person's entry, once"
        );
    }
    assert_eq!(
        member::read_issuer_book(
            &member.places.state,
            &member::node_id(&issuer.places.state).unwrap()
        )
        .len(),
        1,
        "the copy holds the place too"
    );
    ok(peers(&issuer.places, &args(&["rm", &named_at])));
    let listing = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    assert_eq!(
        named(&listing),
        [(Via::Manual, true)],
        "the issuer's rm leaves the person's entry"
    );
}

#[test]
fn peers_add_rm_and_leave() {
    let world = World::new("leave");
    world.write_config("theme = \"Nord\"\n");
    ok(init(
        &world.places,
        &args(&["--host", "m7.local", "--port", "7300"]),
    ));
    let hosts = || -> Vec<String> {
        read_peers(&world.places.state)
            .unwrap()
            .into_iter()
            .map(|peer| peer.target())
            .collect()
    };
    ok(peers(&world.places, &args(&["add", "lab-mini:7101"])));
    assert_eq!(hosts(), ["lab-mini:7101"]);
    ok(peers(&world.places, &args(&["add", "LAB-MINI:7101"])));
    assert_eq!(hosts(), ["lab-mini:7101"], "recorded once");
    assert!(matches!(
        peers(&world.places, &args(&["add", "lab mini"])),
        Err(Failure::Usage(_))
    ));
    ok(peers(&world.places, &args(&["rm", "lab-mini"])));
    assert_eq!(hosts(), Vec::<String>::new());
    assert!(matches!(
        peers(&world.places, &args(&["rm", "lab-mini"])),
        Err(Failure::Failed(_))
    ));

    let left = ok(leave(&world.places, &[]));
    assert!(
        left.contains("This machine issued the fleet") && !left.contains("is not told"),
        "{left}"
    );
    for gone in [KEY_FILE, CODE_FILE, PEERS_FILE] {
        assert!(!world.places.state.join(gone).exists(), "{gone} removed");
    }
    assert!(
        member::node_id(&world.places.state).is_ok(),
        "the node id stays"
    );
    assert_eq!(world.config().trim(), "theme = \"Nord\"");
    ok(leave(&world.places, &[]));
    assert_eq!(
        world.config().trim(),
        "theme = \"Nord\"",
        "nothing to leave changes nothing"
    );
}

#[test]
fn a_join_to_an_issuer_past_the_lan_is_refused_by_its_kind() {
    let code = JoinCode {
        secret: [7; member::SECRET_LEN],
        host: "192.0.2.1".to_owned(),
        port: 7100,
    };
    let secret = FleetSecret::new(&code.secret).unwrap();
    let registration = Registration {
        node: "ab".repeat(16),
        host: "lab-mini".to_owned(),
        port: 7100,
        version: "0.95.0".to_owned(),
        key: None,
    };
    let me = aterm_net::fleet::NodeIdentity::from_seed(&registration.node, &[3; 32]).unwrap();
    let refused = register_with(
        &code,
        (&secret, &me),
        (&Keyring::default(), None),
        &registration,
    );
    assert!(
        matches!(&refused, Err(JoinFailed::Dial(Unreached::NotFleet(_)))),
        "{refused:?}"
    );
    let fresh = World::new("past-the-lan");
    assert!(matches!(
        join(&fresh.places, &Opts::default(), &code.text()),
        Err(Failure::Failed(_))
    ));
    assert!(!fresh.places.state.join(KEY_FILE).exists(), "no key");
    assert!(!fresh.places.config.exists(), "no [fleet]");
}

#[test]
fn every_reason_a_fleet_read_gives_fits_its_row() {
    use aterm_ctl::PeerFault;
    use aterm_net::fleet::FleetError;
    use aterm_net::fleet_member::Stage;
    let at = std::net::SocketAddr::from(([192, 168, 100, 200], 7100));
    let host = "Andrews-MacBook-Pro.local".to_owned();
    let within = Duration::from_secs(10);
    let connect = |errno| Unreached::Connect(at, io::Error::from_raw_os_error(errno));
    let reply = |line: &str| {
        Unreached::Reply(
            at,
            aterm_net::fleet_member::reply_head(line).expect_err("no survey"),
        )
    };
    let broke = |answering, e: io::Error| {
        Unreached::Reply(at, aterm_net::fleet_member::broke_off(answering, &e))
    };
    let eof = || {
        io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "peer closed connection without sending TLS close_notify: \
             https://docs.rs/rustls/latest/rustls/manual/_03_howto/index.html#unexpected-eof",
        )
    };
    let node = "0123456789abcdef0123456789abcdef".to_owned();
    let reasons = [
        (Unreached::Resolve(host.clone()), PeerFault::NotDialled),
        (
            Unreached::Lookup(host.clone(), within),
            PeerFault::NotDialled,
        ),
        (Unreached::NotFleet(at), PeerFault::NotDialled),
        (connect(libc::ECONNREFUSED), PeerFault::Other),
        (connect(libc::ETIMEDOUT), PeerFault::Other),
        (connect(libc::EHOSTUNREACH), PeerFault::NoRoute),
        (connect(libc::ENETUNREACH), PeerFault::NoNetwork),
        (connect(libc::EPERM), PeerFault::NotPermitted),
        (connect(libc::EACCES), PeerFault::NotPermitted),
        (connect(libc::ECONNRESET), PeerFault::Other),
        (connect(libc::EADDRNOTAVAIL), PeerFault::Other),
        (connect(libc::EHOSTDOWN), PeerFault::Other),
        (connect(libc::ENETDOWN), PeerFault::Other),
        (
            Unreached::Handshake(at, FleetError::ListenerWrongSecret),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(at, FleetError::Refused),
            PeerFault::Other,
        ),
        (Unreached::Handshake(at, FleetError::Alpn), PeerFault::Other),
        (
            Unreached::Handshake(at, FleetError::AddressBusy),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(at, FleetError::Reflection),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(at, FleetError::UnknownOp("survey".to_owned())),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(at, FleetError::UnknownScope("summary".to_owned())),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(at, FleetError::UnknownScope("w".repeat(120))),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(
                at,
                FleetError::Malformed("the listener's proof is not `<scope> <64 hex digits>`"),
            ),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(
                at,
                FleetError::Malformed("the listener's answer is neither `OK` nor `NO <why>`"),
            ),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(
                at,
                FleetError::Tls(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "received corrupt message of type InvalidContentType",
                )),
            ),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(
                at,
                FleetError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the listener hung up without answering this machine's proof",
                )),
            ),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(
                at,
                FleetError::Io(io::Error::from_raw_os_error(libc::ECONNRESET)),
            ),
            PeerFault::Other,
        ),
        (
            Unreached::Handshake(at, FleetError::Timeout(within)),
            PeerFault::Other,
        ),
        (
            reply("ERR denied: the fleet serves only ping, peers"),
            PeerFault::Other,
        ),
        (
            reply(&format!("ERR busy: {}", "w ".repeat(100))),
            PeerFault::Other,
        ),
        (reply(&"x".repeat(200)), PeerFault::Other),
        (broke(false, eof()), PeerFault::Other),
        (broke(true, eof()), PeerFault::Other),
        (
            broke(true, io::Error::from_raw_os_error(libc::ECONNRESET)),
            PeerFault::Other,
        ),
        (
            broke(false, io::Error::from_raw_os_error(libc::EPIPE)),
            PeerFault::Other,
        ),
        (
            broke(
                true,
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "received corrupt message of type InvalidContentType",
                ),
            ),
            PeerFault::Other,
        ),
        (
            Unreached::Deadline(Stage::Connect, within),
            PeerFault::Other,
        ),
        (
            Unreached::Deadline(Stage::Handshake, within),
            PeerFault::Other,
        ),
        (Unreached::Deadline(Stage::Answer, within), PeerFault::Other),
        (
            Unreached::OtherNode {
                at,
                said: node.clone(),
                want: node.chars().rev().collect(),
            },
            PeerFault::Other,
        ),
        (
            Unreached::Impostor {
                at,
                node: node.clone(),
                issuer: false,
            },
            PeerFault::Other,
        ),
        (
            Unreached::Impostor {
                at,
                node: node.clone(),
                issuer: true,
            },
            PeerFault::Other,
        ),
        (
            Unreached::Unproved {
                at,
                node: node.clone(),
            },
            PeerFault::Other,
        ),
        (
            Unreached::Misnamed {
                at,
                proved: node.clone(),
                claimed: "w".repeat(120),
            },
            PeerFault::Other,
        ),
    ];
    for (why, fault) in &reasons {
        let text = why.to_string();
        assert!(
            text.chars().count() <= aterm_ctl::UNANSWERED_REASON_CHARS,
            "{} > {}: {text}",
            text.chars().count(),
            aterm_ctl::UNANSWERED_REASON_CHARS
        );
        assert_eq!(fault_of(why), *fault, "{why:?}");
    }
}

#[test]
fn a_failed_join_is_advised_by_its_kind() {
    let at = std::net::SocketAddr::from(([192, 168, 1, 23], 7100));
    let dial =
        |errno| JoinFailed::Dial(Unreached::Connect(at, io::Error::from_raw_os_error(errno)));
    let cases = [
        (
            JoinFailed::Registration(member::NOT_THE_ISSUER.to_owned()),
            Advice::NoIssuer,
        ),
        (
            JoinFailed::Dial(Unreached::NotFleet(at)),
            Advice::PastTheLan,
        ),
        (dial(libc::EPERM), Advice::Sandbox),
        (dial(libc::EACCES), Advice::Sandbox),
        (dial(libc::EHOSTUNREACH), Advice::LocalNetwork),
        (dial(libc::ENETUNREACH), Advice::NoNetwork),
        (JoinFailed::OtherKey("ab".repeat(16)), Advice::IssuerKey),
        (JoinFailed::Unproved("ab".repeat(16)), Advice::IssuerKey),
        (
            JoinFailed::OtherIssuer {
                bound: "ab".repeat(16),
                proved: Some("cd".repeat(16)),
            },
            Advice::BoundIssuer,
        ),
        (
            JoinFailed::OtherIssuer {
                bound: "ab".repeat(16),
                proved: None,
            },
            Advice::BoundUnproved,
        ),
        (
            JoinFailed::Registration(format!("ERR node abababab is bound … {}", member::REBIND)),
            Advice::NodeBound,
        ),
        (
            JoinFailed::Registration(format!(
                "ERR node abababab was recorded from 10.0.0.8 {}, and its key is bound only \
                 from there: `aterm fleet peers rm abababab` {}",
                member::BEFORE_KEYS,
                member::REBIND
            )),
            Advice::BeforeKeys,
        ),
        (dial(libc::ECONNREFUSED), Advice::CheckTheIssuer),
        (
            JoinFailed::Registration("ERR the book is full".to_owned()),
            Advice::CheckTheIssuer,
        ),
    ];
    for (failed, advice) in cases {
        assert_eq!(failed.advice(), advice, "{failed:?}");
    }
}

#[test]
fn a_persons_entry_at_a_copys_place_is_read_beside_it() {
    let world = World::new("placekey");
    world.write_config("[[fleet.peers]]\nhost = \"m7\"\n");
    member::ensure_private_dir(&world.places.state).unwrap();
    let issuer_node = "11".repeat(16);
    let n = NodeIdentity::from_seed(&"c3".repeat(16), &[6; 32]).unwrap();
    member::update_peers(&world.places.state, |book| {
        book.push(Peer {
            node: Some(issuer_node.clone()),
            via: Via::Issuer,
            ..manual_peer("m9.local".to_owned(), member::DEFAULT_PORT)
        });
        Ok(())
    })
    .unwrap();
    let copy = Peer {
        node: Some(n.node().to_owned()),
        key: Some(n.key()),
        via: Via::IssuerBook,
        ..manual_peer("m7.local".to_owned(), member::DEFAULT_PORT)
    };
    let by_hand = Peer {
        via: Via::IssuerBook,
        ..manual_peer("m7.local".to_owned(), member::DEFAULT_PORT)
    };
    member::hold_book(&world.places.state)
        .unwrap()
        .keep_issuer_book(Some((&issuer_node, &[copy.clone(), by_hand])))
        .unwrap();
    let known = known_peers(&world.places).unwrap_or_else(|f| panic!("{f:?}"));
    let targets: Vec<Peer> = known.peers.iter().map(|(peer, _)| peer.clone()).collect();
    assert_eq!(
        targets.iter().map(|peer| peer.via).collect::<Vec<_>>(),
        [Via::Issuer, Via::Manual, Via::IssuerBook],
        "the copy that names a node, beside the person's entry"
    );
    assert_eq!(targets[2], copy);
    assert_eq!(known.keys.bound(n.node()), Some(n.key()));
}

#[test]
fn a_survey_answer_is_unproven_unless_its_node_key_is_bound() {
    let peer = manual_peer("m7".to_owned(), member::DEFAULT_PORT);
    let answered = |proven: bool| {
        Ok(member::Surveyed {
            scope: FleetScope::Facts,
            addr: std::net::SocketAddr::from(([10, 0, 0, 7], member::DEFAULT_PORT)),
            node: Some("c3".repeat(16)),
            proven,
            key: None,
            lines: Vec::new(),
        })
    };
    assert!(peer_survey(&peer, answered(false)).unproven);
    assert!(!peer_survey(&peer, answered(true)).unproven);
    let unreached = Unreached::Local("no".to_owned());
    assert!(!peer_survey(&peer, Err(unreached)).unproven);
}

#[test]
fn peers_takes_a_timeout_up_to_the_surveys_longest() {
    for secs in ["0.5", "10"] {
        assert!(check_within(secs).is_ok(), "{secs}");
    }
    for secs in ["0", "-1", "10.5", "soon"] {
        assert!(
            matches!(check_within(secs), Err(Failure::Usage(_))),
            "{secs}"
        );
    }
    let world = World::new("timeout");
    assert!(matches!(
        peers(&world.places, &args(&["--timeout"])),
        Err(Failure::Usage(_))
    ));
}

#[test]
fn allow_drive_names_only_the_nodes_the_book_binds() {
    let world = World::new("allow");
    world.write_config("theme = \"Nord\"\n");
    let refused = |result: Result<String, Failure>| match result {
        Err(Failure::Failed(why) | Failure::Usage(why)) => why,
        Ok(report) => panic!("{report}"),
    };
    let none = refused(allow(&world.places, &args(&["content"])));
    assert!(none.contains("in no fleet"), "{none}");
    let drives = drive_fleet_in(&world.places, "lab-mini", Duration::from_secs(1))
        .err()
        .expect("in no fleet");
    assert!(drives.why.contains("in no fleet"), "{drives}");
    assert!(!drives.again, "{drives:?}");

    ok(init(&world.places, &args(&["--host", "127.0.0.1"])));
    let bound = "b0".repeat(16);
    let unbound = "c1".repeat(16);
    let key = NodeIdentity::from_seed(&bound, &[3; 32]).unwrap().key();
    let peer = |node: &str, key: Option<NodeKey>| Peer {
        node: Some(node.to_owned()),
        key,
        key_from: None,
        host: "127.0.0.1".to_owned(),
        addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
        port: closed_port(),
        version: None,
        seen_ms: None,
        via: Via::Joined,
        fleet: None,
    };
    member::update_peers(&world.places.state, |book| {
        book.push(peer(&bound, Some(key)));
        book.push(peer(&unbound, None));
        Ok(())
    })
    .unwrap();

    for (words, why) in [
        (&["drive"][..], "names the nodes"),
        (&["facts", "--from", "b0b0b0b0"], "drive level alone"),
        (&["steer"], "no such level"),
        (&["drive", "--from"], "takes a host"),
    ] {
        let said = refused(allow(&world.places, &args(words)));
        assert!(said.contains(why), "{words:?}: {said}");
    }
    let said = refused(allow(
        &world.places,
        &args(&["drive", "--from", "d2d2d2d2"]),
    ));
    assert!(said.contains("names no peer"), "{said}");
    ok(peers(
        &world.places,
        &args(&["add", "lab-mini.invalid:7101"]),
    ));
    for (word, why) in [("c1c1c1c1", "binds no key"), ("lab-mini", "named by hand")] {
        let said = refused(allow(&world.places, &args(&["drive", "--from", word])));
        assert!(said.contains(why), "{word}: {said}");
    }
    for (word, why) in [
        ("10.9.9.9:7100", "no Mac this machine's book records"),
        ("lab-mini", "named by hand"),
    ] {
        let refused = drive_fleet_in(&world.places, word, Duration::from_secs(1))
            .err()
            .unwrap_or_else(|| panic!("{word} is driven"));
        assert!(refused.why.contains(why), "{word}: {refused}");
        assert!(!refused.again, "{word}: {refused:?}");
    }
    assert_eq!(
        fleet_table(&world.config()).unwrap().level(),
        Some("facts"),
        "a refusal writes nothing"
    );

    ok(allow(&world.places, &args(&["drive", "--from=b0b0b0b0"])));
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(table.level(), Some(aterm_net::fleet::DRIVE_LEVEL));
    let granted = format!("{bound}:{}", key.hex());
    assert_eq!(table.drive_from, vec![granted.clone()]);
    assert!(world.config().contains("theme = \"Nord\""));
    let again = ok(allow(&world.places, &args(&["DRIVE", "--from", &bound])));
    assert!(again.contains("already"), "{again}");
    let rekey = NodeIdentity::from_seed(&bound, &[4; 32]).unwrap().key();
    member::update_peers(&world.places.state, |book| {
        for entry in book.iter_mut() {
            if entry.node.as_deref() == Some(bound.as_str()) {
                entry.key = Some(rekey);
            }
        }
        Ok(())
    })
    .unwrap();
    ok(allow(&world.places, &args(&["drive", "--from", &bound])));
    assert_eq!(
        fleet_table(&world.config()).unwrap().drive_from,
        vec![format!("{bound}:{}", rekey.hex())]
    );
    ok(peers(
        &world.places,
        &args(&["rm", "lab-mini.invalid:7101"]),
    ));
    assert_eq!(fleet_table(&world.config()).unwrap().drive_from.len(), 1);
    let removed = ok(peers(&world.places, &args(&["rm", &bound])));
    assert!(removed.contains("its grant"), "{removed}");
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from),
        (Some("content"), Vec::new())
    );
    member::update_peers(&world.places.state, |book| {
        book.push(peer(&bound, Some(key)));
        Ok(())
    })
    .unwrap();

    ok(allow(&world.places, &args(&["content"])));
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from),
        (Some("content"), Vec::new())
    );

    ok(allow(&world.places, &args(&["drive", "--from", &bound])));
    ok(leave(&world.places, &[]));
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(
        (table.listen, table.allow, table.drive_from),
        (None, None, Vec::new())
    );
}

#[test]
fn a_join_of_another_fleet_keeps_no_drive_grant() {
    let issuer_of = |tag: &str| {
        let world = World::new(tag);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        ok(init(
            &world.places,
            &args(&["--host", "127.0.0.1", "--port", &port]),
        ));
        let code = std::fs::read_to_string(world.places.state.join(CODE_FILE)).unwrap();
        let serving = Listener::serve(listener, &world.places.state);
        (world, code, serving)
    };
    let (first, first_code, _first) = issuer_of("grant-first");
    let (_other, other_code, _other_serving) = issuer_of("grant-other");
    let member = World::new("grant-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let issuer_node = member::node_id(&first.places.state).unwrap();
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    let granted = fleet_table(&member.config()).unwrap();
    assert_eq!(granted.drive_from.len(), 1, "{granted:?}");

    let again = ok(join(&member.places, &opts, &first_code));
    assert!(!again.contains("no Mac drives this one"), "{again}");
    assert_eq!(
        fleet_table(&member.config()).unwrap(),
        granted,
        "a join again of the same fleet keeps its grants"
    );

    let moved = ok(join(&member.places, &opts, &other_code));
    assert!(moved.contains("no Mac drives this one"), "{moved}");
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from),
        (Some("content"), Vec::new())
    );
}

#[test]
fn a_join_of_another_fleet_that_fails_after_its_issuer_recorded_it_finishes_on_a_join_again() {
    let issuer_of = |tag: &str| {
        let world = World::new(tag);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        ok(init(
            &world.places,
            &args(&["--host", "127.0.0.1", "--port", &port]),
        ));
        let code = std::fs::read_to_string(world.places.state.join(CODE_FILE)).unwrap();
        let serving = Listener::serve(listener, &world.places.state);
        (world, code, serving)
    };
    let (first, first_code, _first) = issuer_of("moving-first");
    let (other, other_code, _other) = issuer_of("moving-other");
    let member = World::new("moving-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let issuer_here = || {
        read_peers(&member.places.state)
            .unwrap()
            .into_iter()
            .filter(|peer| peer.via == Via::Issuer)
            .map(|peer| peer.node)
            .collect::<Vec<_>>()
    };

    let book = member.places.state.join(PEERS_FILE);
    let held = std::fs::read(&book).unwrap();
    std::fs::remove_file(&book).unwrap();
    std::fs::create_dir(&book).unwrap();
    let refused = match join(&member.places, &opts, &other_code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: its book cannot be read"),
    };
    assert!(refused.contains("before it dials the issuer"), "{refused}");
    assert_eq!(read_peers(&other.places.state).unwrap(), Vec::new());
    std::fs::remove_dir(&book).unwrap();
    write_private(&member.places.state, PEERS_FILE, &held).unwrap();

    let set_mode = |bits| {
        std::fs::set_permissions(&member.places.state, std::fs::Permissions::from_mode(bits))
            .unwrap();
    };
    set_mode(0o500);
    let failed = join(&member.places, &opts, &other_code);
    set_mode(0o700);
    let failed = match failed {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: its book cannot be written"),
    };
    assert!(failed.contains("has recorded this machine"), "{failed}");
    assert_eq!(read_peers(&other.places.state).unwrap().len(), 1);
    assert_eq!(
        (read_key_file(&member.places.state).unwrap(), issuer_here()),
        (
            read_key_file(&first.places.state).unwrap(),
            vec![Some(member::node_id(&first.places.state).unwrap())]
        ),
        "the old fleet's key, beside its issuer"
    );

    ok(join(&member.places, &opts, &other_code));
    assert_eq!(
        (read_key_file(&member.places.state).unwrap(), issuer_here()),
        (
            read_key_file(&other.places.state).unwrap(),
            vec![Some(member::node_id(&other.places.state).unwrap())]
        ),
        "the join again finishes it"
    );
    assert_eq!(read_peers(&other.places.state).unwrap().len(), 1);
}

#[test]
fn a_join_of_another_fleet_whose_key_cannot_be_written_leaves_the_book_as_it_was() {
    let issuer_of = |tag: &str| {
        let world = World::new(tag);
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        ok(init(
            &world.places,
            &args(&["--host", "127.0.0.1", "--port", &port]),
        ));
        let code = std::fs::read_to_string(world.places.state.join(CODE_FILE)).unwrap();
        let serving = Listener::serve(listener, &world.places.state);
        (world, code, serving)
    };
    let (_first, first_code, _first_serving) = issuer_of("unkeyed-first");
    let (other, other_code, _other_serving) = issuer_of("unkeyed-other");
    let member = World::new("unkeyed-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let book_before = read_peers(&member.places.state).unwrap();
    assert!(
        book_before.iter().any(|peer| peer.via == Via::Issuer),
        "{book_before:?}"
    );

    let key = member.places.state.join(KEY_FILE);
    let held = std::fs::read(&key).unwrap();
    std::fs::remove_file(&key).unwrap();
    std::fs::create_dir(&key).unwrap();
    let failed = match join(&member.places, &opts, &other_code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: its key cannot be replaced"),
    };
    assert!(failed.contains("has recorded this machine"), "{failed}");
    assert_eq!(read_peers(&other.places.state).unwrap().len(), 1);

    std::fs::remove_dir(&key).unwrap();
    write_private(&member.places.state, KEY_FILE, &held).unwrap();
    assert_eq!(
        read_peers(&member.places.state).unwrap(),
        book_before,
        "the old fleet's book, its issuer bound"
    );
    ok(join(&member.places, &opts, &other_code));
    let issuers: Vec<_> = read_peers(&member.places.state)
        .unwrap()
        .into_iter()
        .filter(|peer| peer.via == Via::Issuer)
        .map(|peer| peer.node)
        .collect();
    assert_eq!(
        (read_key_file(&member.places.state).unwrap(), issuers),
        (
            read_key_file(&other.places.state).unwrap(),
            vec![Some(member::node_id(&other.places.state).unwrap())]
        ),
        "the join again finishes it"
    );
}

fn issuing(tag: &str) -> (World, String, Listener) {
    let world = World::new(tag);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    ok(init(
        &world.places,
        &args(&["--host", "127.0.0.1", "--port", &port]),
    ));
    let code = std::fs::read_to_string(world.places.state.join(CODE_FILE)).unwrap();
    let serving = Listener::serve(listener, &world.places.state);
    (world, code, serving)
}

fn issuer_bound(places: &Places) -> Option<String> {
    let held = read_key_file(&places.state).ok()?;
    bound_issuer(places, &held).unwrap().map(|(node, _)| node)
}

fn with_step_hook<R>(hook: impl FnMut(&str) + 'static, act: impl FnOnce() -> R) -> R {
    AT_STEP.with(|at| *at.borrow_mut() = Some(Box::new(hook)));
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(act));
    AT_STEP.with(|at| *at.borrow_mut() = None);
    done.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn killed_at(step: &'static str, act: impl FnOnce() -> Result<String, Failure>) -> bool {
    let hook = move |at: &str| assert_ne!(at, step, "the join is killed here");
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| with_step_hook(hook, act))).is_err()
}

#[test]
fn a_join_of_another_fleet_killed_before_its_key_leaves_the_old_fleet_whole() {
    let (first, first_code, _first) = issuing("killed-first");
    let (other, other_code, _other) = issuing("killed-other");
    let member = World::new("killed-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let first_node = member::node_id(&first.places.state).unwrap();

    assert!(killed_at("key", || join(
        &member.places,
        &opts,
        &other_code
    )));
    assert_eq!(read_peers(&other.places.state).unwrap().len(), 1);
    assert_eq!(
        (
            read_key_file(&member.places.state).unwrap(),
            issuer_bound(&member.places)
        ),
        (
            read_key_file(&first.places.state).unwrap(),
            Some(first_node.clone())
        ),
        "the old fleet's key, beside its issuer"
    );
    assert!(
        Keyring::read(&member.places.state)
            .bound(&first_node)
            .is_some()
    );

    ok(join(&member.places, &opts, &other_code));
    assert_eq!(
        (
            read_key_file(&member.places.state).unwrap(),
            issuer_bound(&member.places)
        ),
        (
            read_key_file(&other.places.state).unwrap(),
            Some(member::node_id(&other.places.state).unwrap())
        ),
        "the join again finishes it"
    );
}

#[test]
fn a_read_while_a_join_of_another_fleet_writes_its_key_finds_the_old_fleet_whole() {
    let (first, first_code, _first) = issuing("window-first");
    let (_other, other_code, _other_serving) = issuing("window-other");
    let member = World::new("window-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let first_node = member::node_id(&first.places.state).unwrap();
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &first_node]),
    ));
    ok(peers(&member.places, &[]));
    let granted = fleet_table(&member.config()).unwrap();
    assert_eq!(granted.drive_from.len(), 1, "{granted:?}");
    assert!(member::holds_issuer_book(&member.places.state, &first_node));

    let key = member.places.state.join(KEY_FILE);
    let held = std::fs::read(&key).unwrap();
    let elsewhere = Places {
        state: member.places.state.clone(),
        config: member.places.config.clone(),
    };
    let failed = with_step_hook(
        {
            let key = key.clone();
            move |at| {
                if at == "key" {
                    let _ = peers(&elsewhere, &[]);
                    // The key's write fails now.
                    std::fs::remove_file(&key).unwrap();
                    std::fs::create_dir(&key).unwrap();
                }
            }
        },
        || join(&member.places, &opts, &other_code),
    );
    assert!(matches!(failed, Err(Failure::Failed(_))), "{failed:?}");
    std::fs::remove_dir(&key).unwrap();
    write_private(&member.places.state, KEY_FILE, &held).unwrap();
    assert_eq!(
        fleet_table(&member.config()).unwrap(),
        granted,
        "the grant stands"
    );
    assert!(
        member::holds_issuer_book(&member.places.state, &first_node),
        "the copy stands"
    );
    assert_eq!(issuer_bound(&member.places), Some(first_node));
}

#[test]
fn a_join_of_another_fleet_unfinished_on_its_config_takes_the_grants_out_on_a_join_again() {
    let (issuer, code, serving) = issuing("regrant-issuer");
    let member = World::new("regrant-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    assert_eq!(fleet_table(&member.config()).unwrap().drive_from.len(), 1);

    drop(serving);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    ok(init(&issuer.places, &args(&["--rotate", "--port", &port])));
    let rotated = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    let _serving = Listener::serve(listener, &issuer.places.state);

    let lock = member.lock_config();
    let failed = match join(&member.places, &opts, &rotated) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: aterm.toml cannot be written"),
    };
    assert!(failed.contains("has recorded this machine"), "{failed}");
    assert!(
        failed.contains("[fleet] is the one this machine had") && !failed.contains("no [fleet]"),
        "{failed}"
    );
    assert_eq!(
        Keyring::read(&member.places.state).bound(&issuer_node),
        None,
        "the old grant is backed by no book"
    );

    std::fs::remove_dir(&lock).unwrap();
    let again = ok(join(&member.places, &opts, &rotated));
    assert!(again.contains("no Mac drives this one"), "{again}");
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from),
        (Some("content"), Vec::new())
    );
}

#[test]
fn a_join_unfinished_on_its_config_after_a_leave_takes_the_grants_out_on_a_join_again() {
    let (issuer, code, serving) = issuing("unheld-issuer");
    let member = World::new("unheld-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    let lock = member.lock_config();
    assert!(
        leave(&member.places, &[]).is_err(),
        "aterm.toml cannot be written"
    );
    assert!(read_key_file(&member.places.state).is_err());
    assert_eq!(fleet_table(&member.config()).unwrap().drive_from.len(), 1);

    drop(serving);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    ok(init(&issuer.places, &args(&["--rotate", "--port", &port])));
    let rotated = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    let _serving = Listener::serve(listener, &issuer.places.state);
    let failed = match join(&member.places, &opts, &rotated) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: aterm.toml cannot be written"),
    };
    assert!(failed.contains("has recorded this machine"), "{failed}");

    std::fs::remove_dir(&lock).unwrap();
    let again = ok(join(&member.places, &opts, &rotated));
    assert!(again.contains("no Mac drives this one"), "{again}");
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from),
        (Some("content"), Vec::new())
    );
}

#[test]
fn an_init_after_a_leave_stopped_on_its_config_takes_the_grants_out() {
    let (issuer, code, _serving) = issuing("init-left-issuer");
    let member = World::new("init-left-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    let granted = member.config();
    let lock = member.lock_config();
    assert!(
        leave(&member.places, &[]).is_err(),
        "aterm.toml cannot be written"
    );
    assert_eq!(fleet_table(&member.config()).unwrap().drive_from.len(), 1);

    let port = closed_port().to_string();
    let new_fleet = args(&["--host", "127.0.0.1", "--port", &port]);
    assert!(matches!(
        init(&member.places, &new_fleet),
        Err(Failure::Failed(_))
    ));
    assert!(
        read_key_file(&member.places.state).is_err(),
        "nothing of the new fleet is written"
    );

    std::fs::remove_dir(&lock).unwrap();
    let report = ok(init(&member.places, &new_fleet));
    assert!(report.contains("no Mac drives this one"), "{report}");
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from),
        (Some("content"), Vec::new())
    );

    member.write_config(&granted);
    ok(init(&member.places, &args(&["--rotate"])));
    assert_eq!(
        fleet_table(&member.config()).unwrap().drive_from,
        fleet_table(&granted).unwrap().drive_from,
        "a new secret of the same fleet keeps them"
    );
}

fn read_as_older(state: &std::path::Path) -> Vec<Peer> {
    std::fs::read_to_string(state.join(PEERS_FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(Peer::parse)
        .filter(|peer| peer.via != Via::IssuerBook)
        .map(|peer| Peer {
            fleet: None,
            ..peer
        })
        .collect()
}

#[test]
fn a_join_of_another_fleet_unfinished_leaves_an_older_aterm_the_issuer_of_neither() {
    let (first, first_code, _first_serving) = issuing("older-first");
    let (other, other_code, _other_serving) = issuing("older-other");
    let member = World::new("older-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    ok(peers(&member.places, &args(&["add", "127.0.0.1:7199"])));
    // A grant the join takes out: an edit of aterm.toml it cannot write.
    ok(allow(
        &member.places,
        &args(&[
            "drive",
            "--from",
            &member::node_id(&first.places.state).unwrap(),
        ]),
    ));
    let lock = member.lock_config();
    assert!(join(&member.places, &opts, &other_code).is_err());
    let older = read_as_older(&member.places.state);
    assert!(
        older.iter().all(|peer| peer.via == Via::Manual),
        "an older aterm reads an issuer beside the new key: {older:?}"
    );
    // An older aterm's edit of the book: every line written back unmarked.
    let mut text = String::from("# The fleet's peers, one per line.\n");
    for peer in older {
        text.push_str(&peer.line());
        text.push('\n');
    }
    write_private(&member.places.state, PEERS_FILE, text.as_bytes()).unwrap();

    std::fs::remove_dir(&lock).unwrap();
    ok(join(&member.places, &opts, &other_code));
    assert_eq!(
        (
            read_key_file(&member.places.state).unwrap(),
            issuer_bound(&member.places)
        ),
        (
            read_key_file(&other.places.state).unwrap(),
            Some(member::node_id(&other.places.state).unwrap())
        ),
        "the join again finishes it"
    );
    assert_eq!(
        read_as_older(&member.places.state).len(),
        read_peers(&member.places.state).unwrap().len(),
        "the book holds no other fleet's entries"
    );
}

#[test]
fn an_older_aterms_edit_never_comes_between_a_joins_marks_and_its_key() {
    let (first, first_code, _first_serving) = issuing("between-first");
    let (other, other_code, _other_serving) = issuing("between-other");
    let member = World::new("between-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let first_node = member::node_id(&first.places.state).unwrap();
    // A grant the join takes out: an edit of aterm.toml it cannot write.
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &first_node]),
    ));
    let lock = member.lock_config();
    let state = member.places.state.clone();
    let failed = with_step_hook(
        move |at| {
            // An older aterm's edit of the book under its lock: every line written back unmarked.
            if at == "key"
                && let Ok(_book) = member::hold_book(&state)
            {
                let mut text = String::from("# The fleet's peers, one per line.\n");
                for peer in read_as_older(&state) {
                    text.push_str(&peer.line());
                    text.push('\n');
                }
                write_private(&state, PEERS_FILE, text.as_bytes()).unwrap();
            }
        },
        || join(&member.places, &opts, &other_code),
    );
    assert!(
        matches!(failed, Err(Failure::Failed(_))),
        "{failed:?}: aterm.toml cannot be written"
    );
    assert_eq!(
        (
            issuer_bound(&member.places),
            Keyring::read(&member.places.state).bound(&first_node)
        ),
        (None, None),
        "the old issuer binds nothing under the new key"
    );

    std::fs::remove_dir(&lock).unwrap();
    ok(join(&member.places, &opts, &other_code));
    assert_eq!(
        issuer_bound(&member.places),
        Some(member::node_id(&other.places.state).unwrap())
    );
}

#[test]
fn a_join_of_another_fleet_killed_after_its_key_is_finished_by_a_join_again() {
    let (first, first_code, _first_serving) = issuing("after-key-first");
    let (other, other_code, _other_serving) = issuing("after-key-other");
    let member = World::new("after-key-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let first_node = member::node_id(&first.places.state).unwrap();

    assert!(killed_at("drop", || join(
        &member.places,
        &opts,
        &other_code
    )));
    assert_eq!(
        (
            read_key_file(&member.places.state).unwrap(),
            issuer_bound(&member.places),
            Keyring::read(&member.places.state).bound(&first_node)
        ),
        (read_key_file(&other.places.state).unwrap(), None, None),
        "the new fleet's key, beside no issuer"
    );

    ok(join(&member.places, &opts, &other_code));
    assert_eq!(
        issuer_bound(&member.places),
        Some(member::node_id(&other.places.state).unwrap()),
        "the join again finishes it"
    );
    assert_eq!(
        read_as_older(&member.places.state).len(),
        read_peers(&member.places.state).unwrap().len(),
        "the book holds no other fleet's entries"
    );
}

#[test]
fn an_unfinished_join_says_what_the_book_binds() {
    let (first, first_code, _first_serving) = issuing("says-first");
    let (_other, other_code, _other_serving) = issuing("says-other");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    let first_node = member::node_id(&first.places.state).unwrap();

    let member = World::new("says-unlistened");
    ok(join(&member.places, &opts, &first_code));
    member.write_config("");
    let _lock = member.lock_config();
    let failed = match join(&member.places, &opts, &first_code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: aterm.toml cannot be written"),
    };
    assert!(failed.contains("no [fleet] listen"), "{failed}");
    assert!(
        failed.contains("the book's entry of the issuer is as it was")
            && !failed.contains("does not name the issuer"),
        "{failed}"
    );
    assert_eq!(issuer_bound(&member.places), Some(first_node.clone()));

    let member = World::new("says-back");
    ok(join(&member.places, &opts, &first_code));
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &first_node]),
    ));
    let _lock = member.lock_config();
    assert!(join(&member.places, &opts, &other_code).is_err());
    let failed = match join(&member.places, &opts, &first_code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: aterm.toml cannot be written"),
    };
    assert!(
        failed.contains("does not name the issuer") && failed.contains("for no one"),
        "{failed}"
    );
    assert_eq!(issuer_bound(&member.places), None, "{failed}");
    assert_eq!(
        Keyring::read(&member.places.state).bound(&first_node),
        None,
        "the grant is backed by no book"
    );
}

#[test]
fn a_join_an_older_aterm_finished_still_reaches_only_the_issuer_it_binds() {
    let (first, first_code, _first_serving) = issuing("left-first");
    let (other, other_code, _other_serving) = issuing("left-other");
    let member = World::new("left-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    let first_node = member::node_id(&first.places.state).unwrap();
    // A grant the join takes out: an edit of aterm.toml it cannot write.
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &first_node]),
    ));
    let granted = member.config();
    let lock = member.lock_config();
    assert!(join(&member.places, &opts, &other_code).is_err());
    std::fs::remove_dir(&lock).unwrap();
    // An older aterm finishes the join: aterm.toml's grants kept, the issuer recorded unmarked.
    ok(join(&member.places, &opts, &other_code));
    member.write_config(&granted);
    let mut text = String::from("# The fleet's peers, one per line.\n");
    for peer in read_as_older(&member.places.state) {
        text.push_str(&peer.line());
        text.push('\n');
    }
    write_private(&member.places.state, PEERS_FILE, text.as_bytes()).unwrap();
    ok(join(&member.places, &opts, &other_code));
    assert_eq!(
        (
            fleet_table(&member.config()).unwrap().drive_from,
            Keyring::read(&member.places.state).bound(&first_node)
        ),
        (fleet_table(&granted).unwrap().drive_from, None),
        "a join again keeps the old fleet's grant, which no book backs"
    );
    ok(peers(&member.places, &[]));
    assert!(
        fleet_table(&member.config()).unwrap().drive_from.is_empty(),
        "the next proven read takes it out"
    );
    let other_node = member::node_id(&other.places.state).unwrap();
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &other_node]),
    ));
    let config = member.config();

    let stranger = World::new("left-stranger");
    ok(init(
        &stranger.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let secret = JoinCode::parse(&other_code).unwrap().secret;
    member::hold_book(&stranger.places.state)
        .unwrap()
        .write_key(&secret)
        .unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let there = JoinCode {
        secret,
        host: "127.0.0.1".to_owned(),
        port: listener.local_addr().unwrap().port(),
    }
    .text();
    let _stranger_serving = Listener::serve(listener, &stranger.places.state);
    let refused = match join(&member.places, &opts, &there) {
        Err(Failure::Failed(why)) => why,
        result => panic!("{result:?} should be refused"),
    };
    assert!(
        refused.contains(&format!(
            "for the issuer this Mac binds (node {})",
            short(&other_node)
        )),
        "{refused}"
    );
    assert_eq!(member.config(), config, "the grant stands");
}

#[test]
fn a_join_stopped_on_its_book_says_it_took_the_grants_out() {
    let (first, first_code, _first_serving) = issuing("book-step-first");
    let (_other, other_code, _other_serving) = issuing("book-step-other");
    let member = World::new("book-step-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &first_code));
    ok(allow(
        &member.places,
        &args(&[
            "drive",
            "--from",
            &member::node_id(&first.places.state).unwrap(),
        ]),
    ));
    let book = member.places.state.join(PEERS_FILE);
    let failed = with_step_hook(
        {
            let book = book.clone();
            move |at| {
                if at == "book" {
                    // The book cannot be read now.
                    let _ = std::fs::remove_file(&book);
                    std::fs::create_dir(&book).unwrap();
                }
            }
        },
        || join(&member.places, &opts, &other_code),
    );
    let failed = match failed {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: the book cannot be read"),
    };
    assert!(failed.contains("no Mac drives this one"), "{failed}");
    assert!(fleet_table(&member.config()).unwrap().drive_from.is_empty());
    std::fs::remove_dir(&book).unwrap();
    ok(join(&member.places, &opts, &other_code));
}

struct Wire {
    input: std::io::Cursor<Vec<u8>>,
    output: Vec<u8>,
}

impl std::io::Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf)
    }
}

impl std::io::Write for Wire {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.output.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_leave_beside_an_edit_of_the_book_leaves_no_book() {
    let issuer = World::new("leave-edit");
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    let state = issuer.places.state.clone();
    // Inside an edit of the book: the leave waits it out, and past the lock's wait removes nothing.
    let during = member::update_peers(&state, |book| {
        member::record(book, manual_peer("127.0.0.1".to_owned(), 7199))?;
        Ok(leave(&issuer.places, &[]))
    })
    .unwrap();
    assert!(during.is_err(), "{during:?}");
    assert!(read_key_file(&state).is_ok(), "nothing was removed");

    let reg = Registration {
        node: "cd".repeat(16),
        host: "lab-mini".to_owned(),
        port: 58054,
        version: "0.105.0".to_owned(),
        key: None,
    };
    let mut wire = Wire {
        input: std::io::Cursor::new(reg.line().into_bytes()),
        output: Vec::new(),
    };
    let node = member::node_id(&state).unwrap();
    let secret = read_key_file(&state).unwrap();
    // The leave comes after the registration's check of the code, before its edit.
    let recorded = serve_join(
        &mut wire,
        (IpAddr::from(Ipv4Addr::LOCALHOST), None),
        (&state, &secret),
        || {
            ok(leave(&issuer.places, &[]));
            Ok(Issuer {
                node,
                host: "127.0.0.1".to_owned(),
                port: 7100,
                version: "0.95.0".to_owned(),
            })
        },
        7,
    )
    .unwrap();
    assert!(recorded.is_err(), "{recorded:?}");
    assert!(
        std::fs::symlink_metadata(state.join(PEERS_FILE)).is_err(),
        "the book is gone"
    );
}

#[test]
fn a_leave_between_a_joins_steps_leaves_nothing_of_the_fleet_written_after_it() {
    for step in ["config", "book"] {
        let (issuer, code, _serving) = issuing(&format!("between-{step}-issuer"));
        let member = World::new(&format!("between-{step}-member"));
        let opts = Opts {
            port: Some(closed_port()),
            host: Some("127.0.0.1".to_owned()),
            ..Opts::default()
        };
        let elsewhere = Places {
            state: member.places.state.clone(),
            config: member.places.config.clone(),
        };
        let failed = with_step_hook(
            move |at| {
                if at == step {
                    ok(leave(&elsewhere, &[]));
                }
            },
            || join(&member.places, &opts, &code),
        );
        let failed = match failed {
            Err(Failure::Failed(why)) => why,
            other => panic!("{step}: {other:?} should be refused"),
        };
        assert!(
            failed.contains("an `aterm fleet leave`"),
            "{step}: {failed}"
        );
        assert!(read_key_file(&member.places.state).is_err(), "{step}");
        assert_eq!(
            fleet_table(&member.config()).unwrap().listen,
            None,
            "{step}: no [fleet] for a fleet no longer held"
        );
        assert!(
            std::fs::symlink_metadata(member.places.state.join(PEERS_FILE)).is_err(),
            "{step}: no book names the issuer"
        );

        ok(join(&member.places, &opts, &code));
        assert_eq!(
            issuer_bound(&member.places),
            Some(member::node_id(&issuer.places.state).unwrap()),
            "{step}: a join again joins it"
        );
    }
}

#[test]
fn a_join_again_a_leave_comes_into_while_it_dials_keeps_no_grant() {
    // The leave stopped short of aterm.toml or not: the join writes the level a later join would.
    for (stopped, level) in [(false, "facts"), (true, "content")] {
        let (issuer, code, _serving) = issuing(&format!("dial-leave-{stopped}-issuer"));
        let member = World::new(&format!("dial-leave-{stopped}-member"));
        let opts = Opts {
            port: Some(closed_port()),
            host: Some("127.0.0.1".to_owned()),
            ..Opts::default()
        };
        ok(join(&member.places, &opts, &code));
        let issuer_node = member::node_id(&issuer.places.state).unwrap();
        ok(allow(
            &member.places,
            &args(&["drive", "--from", &issuer_node]),
        ));
        let elsewhere = Places {
            state: member.places.state.clone(),
            config: member.places.config.clone(),
        };
        let lock = member
            .places
            .config
            .with_file_name(".aterm.toml.aterm-write.lock");
        let joined = ok(with_step_hook(
            move |at| {
                if at == "lock" && stopped {
                    let _ = std::fs::remove_file(&lock);
                    std::fs::create_dir(&lock).unwrap();
                    assert!(
                        leave(&elsewhere, &[]).is_err(),
                        "aterm.toml cannot be written"
                    );
                    std::fs::remove_dir(&lock).unwrap();
                } else if at == "lock" {
                    ok(leave(&elsewhere, &[]));
                }
            },
            || join(&member.places, &opts, &code),
        ));
        let table = fleet_table(&member.config()).unwrap();
        assert!(table.listen.is_some(), "{stopped}: {joined}");
        assert_eq!(
            (table.level(), table.drive_from),
            (Some(level), Vec::new()),
            "{stopped}: as a join after the leave:\n{joined}"
        );
        assert_eq!(
            joined.contains("allow      facts"),
            level == "facts",
            "{stopped}: {joined}"
        );
        assert_eq!(issuer_bound(&member.places), Some(issuer_node), "{stopped}");
    }
}

#[test]
fn a_join_again_a_leave_and_an_unfinished_join_come_into_keeps_no_grant() {
    // Before its first take of the book's lock, or between two: each leaves this fleet's key and
    // a book that names no issuer.
    for step in ["lock", "config", "book"] {
        let (issuer, code, _serving) = issuing(&format!("unfinished-{step}-issuer"));
        let member = World::new(&format!("unfinished-{step}-member"));
        let opts = Opts {
            port: Some(closed_port()),
            host: Some("127.0.0.1".to_owned()),
            ..Opts::default()
        };
        ok(join(&member.places, &opts, &code));
        let issuer_node = member::node_id(&issuer.places.state).unwrap();
        ok(allow(
            &member.places,
            &args(&["drive", "--from", &issuer_node]),
        ));
        let (state, config) = (member.places.state.clone(), member.places.config.clone());
        let lock = config.with_file_name(".aterm.toml.aterm-write.lock");
        let (port, again) = (opts.port, code.clone());
        let joined = with_step_hook(
            move |at| {
                if at != step {
                    return;
                }
                let places = Places {
                    state: state.clone(),
                    config: config.clone(),
                };
                let _ = std::fs::remove_file(&lock);
                std::fs::create_dir(&lock).unwrap();
                assert!(leave(&places, &[]).is_err(), "aterm.toml cannot be written");
                std::fs::remove_dir(&lock).unwrap();
                let again = again.clone();
                let stopped = std::thread::spawn(move || {
                    let opts = Opts {
                        port,
                        host: Some("127.0.0.1".to_owned()),
                        ..Opts::default()
                    };
                    killed_at("config", || join(&places, &opts, &again))
                });
                assert!(
                    stopped.join().unwrap(),
                    "the other join stops at aterm.toml"
                );
            },
            || join(&member.places, &opts, &code),
        );
        // Its grants were kept, so it records no issuer to back them: a join again finishes it.
        let joined = match joined {
            Err(Failure::Failed(why)) if step == "book" => {
                assert!(why.contains("names its issuer no more"), "{why}");
                assert_eq!(issuer_bound(&member.places), None);
                ok(join(&member.places, &opts, &code))
            }
            joined => ok(joined),
        };
        let table = fleet_table(&member.config()).unwrap();
        assert_eq!(
            (table.level(), table.drive_from),
            (Some("content"), Vec::new()),
            "{step}: as a join after them:\n{joined}"
        );
        assert_eq!(issuer_bound(&member.places), Some(issuer_node), "{step}");
    }
}

#[test]
fn an_init_a_leave_comes_into_holds_the_fleet_whole_or_none_of_it() {
    // Before the key, an init makes the fleet a leave left none of, and a rotate (of no fleet
    // now) is refused; after it, the leave took the fleet out.
    for (step, rotate) in [
        ("key", false),
        ("key", true),
        ("config", false),
        ("config", true),
    ] {
        let world = World::new(&format!("init-leave-{step}-{rotate}"));
        let port = closed_port().to_string();
        let flags = ["--host", "127.0.0.1", "--port", &port];
        if rotate {
            ok(init(&world.places, &args(&flags)));
        }
        let elsewhere = Places {
            state: world.places.state.clone(),
            config: world.places.config.clone(),
        };
        let inited = with_step_hook(
            move |at| {
                if at == step {
                    ok(leave(&elsewhere, &[]));
                }
            },
            || {
                init(
                    &world.places,
                    &args(if rotate {
                        &["--rotate"][..]
                    } else {
                        &flags[..]
                    }),
                )
            },
        );
        let state = &world.places.state;
        let table = fleet_table(&world.config()).unwrap();
        if step == "key" && rotate {
            assert!(
                matches!(&inited, Err(Failure::Usage(why)) if why.contains("it issues none")),
                "{inited:?}"
            );
            assert!(read_key_file(state).is_err() && !state.join(CODE_FILE).exists());
            assert_eq!(table.listen, None, "as a rotate after the leave");
        } else if step == "key" {
            let inited = ok(inited);
            assert!(
                read_key_file(state).is_ok() && state.join(CODE_FILE).exists(),
                "{rotate}: {inited}"
            );
            assert_eq!(
                (table.listen.is_some(), table.level()),
                (true, Some("facts")),
                "{rotate}: as an init after the leave:\n{inited}"
            );
        } else {
            assert!(
                matches!(&inited, Err(Failure::Failed(why)) if why.contains("no longer held")),
                "{rotate}: {inited:?}"
            );
            assert!(read_key_file(state).is_err(), "{rotate}");
            assert!(!state.join(CODE_FILE).exists(), "{rotate}");
            assert_eq!(
                table.listen, None,
                "{rotate}: no [fleet] for a fleet no longer held"
            );
        }
    }
}

#[test]
fn a_join_while_a_leave_edits_aterm_toml_leaves_no_fleet_half_held() {
    let (_issuer, code, _serving) = issuing("inside-leave-issuer");
    let member = World::new("inside-leave-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let elsewhere = Places {
        state: member.places.state.clone(),
        config: member.places.config.clone(),
    };
    let (dialled, at_lock) = std::sync::mpsc::channel();
    let (go, gone) = std::sync::mpsc::channel();
    // The join holds its node key and the issuer's answer before the leave begins, so only its
    // locked steps meet the leave.
    let mut joining = Some(std::thread::spawn(move || {
        with_step_hook(
            move |at| {
                if at == "lock" {
                    dialled.send(()).unwrap();
                    gone.recv_timeout(HANG).unwrap();
                }
            },
            || join(&elsewhere, &opts, &code),
        )
    }));
    at_lock.recv_timeout(HANG).expect("the join dials");
    let (joins, joined) = std::sync::mpsc::channel();
    let config = member.places.config.clone();
    // The key, the book and [fleet] are out, and the leave holds the book's lock still.
    ok(with_step_hook(
        move |at| {
            if at == "unlisten"
                && let Some(joining) = joining.take()
            {
                let text = std::fs::read_to_string(&config).unwrap();
                assert_eq!(
                    fleet_table(&text).unwrap().listen,
                    None,
                    "the leave edits aterm.toml under the book's lock"
                );
                go.send(()).unwrap();
                joins.send(joining.join().unwrap()).unwrap();
            }
        },
        || leave(&member.places, &[]),
    ));
    let joined = joined.try_recv().expect("the leave edits aterm.toml");
    assert!(
        matches!(&joined, Err(Failure::Failed(why)) if why.contains("has recorded this machine")),
        "{joined:?}: the leave holds the book's lock"
    );
    assert!(read_key_file(&member.places.state).is_err());
    assert_eq!(fleet_table(&member.config()).unwrap().listen, None);
    assert!(
        std::fs::symlink_metadata(member.places.state.join(PEERS_FILE)).is_err(),
        "no book names the issuer"
    );
}

#[test]
fn a_read_that_ends_after_a_leave_keeps_nothing_of_the_fleet() {
    let (issuer, code, _serving) = issuing("left-read-issuer");
    let member = World::new("left-read-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let secret = read_key_file(&member.places.state).unwrap();
    let read = BookRead::Fetched(member::Book {
        node: issuer_node.clone(),
        peers: Vec::new(),
        at: IpAddr::from(Ipv4Addr::LOCALHOST),
        proven: true,
    });
    let own = member::node_id(&member.places.state).unwrap();
    assert_eq!(keep_book(&member.places, &secret, &read, Some(&own)), None);
    assert!(member::holds_issuer_book(
        &member.places.state,
        &issuer_node
    ));

    ok(leave(&member.places, &[]));
    for read in [read, BookRead::NoIssuer] {
        assert_eq!(keep_book(&member.places, &secret, &read, Some(&own)), None);
        assert!(
            std::fs::symlink_metadata(member.places.state.join(ISSUER_BOOK_FILE)).is_err(),
            "{read:?}: the copy the leave took out stays out"
        );
    }
}

#[test]
fn a_members_drive_grant_follows_its_issuers_book() {
    let issuer = World::new("grant-book-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let _serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("grant-book-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    let code = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    ok(join(&member.places, &opts, &code));

    let node = "d4".repeat(16);
    let key = NodeIdentity::from_seed(&node, &[5; 32]).unwrap().key();
    let rekey = NodeIdentity::from_seed(&node, &[6; 32]).unwrap().key();
    let third_port = closed_port();
    let bind = |key: Option<NodeKey>| {
        member::update_peers(&issuer.places.state, |book| {
            book.retain(|peer| peer.node.as_deref() != Some(node.as_str()));
            book.extend(key.map(|key| Peer {
                node: Some(node.clone()),
                key: Some(key),
                key_from: None,
                host: "127.0.0.1".to_owned(),
                addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
                port: third_port,
                version: None,
                seen_ms: None,
                via: Via::Joined,
                fleet: None,
            }));
            Ok(())
        })
        .unwrap();
    };
    let grants = || fleet_table(&member.config()).unwrap().drive_from;
    let granted = |key: NodeKey| vec![format!("{node}:{}", key.hex())];
    let read = || check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    let whole = aterm_ctl::FleetAsk {
        host: None,
        within: HANG,
    };

    bind(Some(key));
    ok(allow(&member.places, &args(&["drive", "--from", &node])));
    assert_eq!(grants(), granted(key));
    assert_eq!(read().note, None, "a book that still binds it");
    assert_eq!(grants(), granted(key), "keeps the grant");

    bind(None);
    let note = read().note.unwrap_or_default();
    assert!(note.contains("its grant"), "{note}");
    assert_eq!(grants(), Vec::<String>::new(), "the issuer took it out");
    bind(Some(key));
    let _ = read();
    assert_eq!(
        grants(),
        Vec::<String>::new(),
        "a join again under the old key is granted nothing"
    );

    ok(allow(&member.places, &args(&["drive", "--from", &node])));
    bind(Some(rekey));
    let surveyed = survey_fleet_in(&member.places, &whole).unwrap_or_else(|why| panic!("{why}"));
    assert!(
        surveyed
            .note
            .as_deref()
            .is_some_and(|n| n.contains("another key")),
        "{:?}",
        surveyed.note
    );
    assert_eq!(grants(), Vec::<String>::new(), "bound afresh");

    bind(Some(key));
    ok(allow(&member.places, &args(&["drive", "--from", &node])));
    assert_eq!(grants(), granted(key), "the key the issuer binds now");

    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let copied = || {
        member::read_issuer_book(&member.places.state, &issuer_node)
            .into_iter()
            .filter_map(|peer| peer.node)
            .collect::<Vec<_>>()
    };
    let removed = ok(peers(&member.places, &args(&["rm", &node])));
    assert!(removed.contains("drives this Mac no more"), "{removed}");
    assert_eq!(grants(), Vec::<String>::new());
    assert_eq!(
        copied(),
        std::slice::from_ref(&node),
        "the copy follows the issuer"
    );
    assert!(
        matches!(
            peers(&member.places, &args(&["rm", &node])),
            Err(Failure::Failed(_))
        ),
        "no grant left: the copy's refusal stands"
    );
}

#[test]
fn a_hand_entry_at_a_copys_place_never_hides_its_node_from_a_grant() {
    let issuer = World::new("shadow-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let _serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("shadow-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    let code = std::fs::read_to_string(issuer.places.state.join(CODE_FILE)).unwrap();
    ok(join(&member.places, &opts, &code));

    let node = "e5".repeat(16);
    let key = NodeIdentity::from_seed(&node, &[8; 32]).unwrap().key();
    let [third_port, alone_port] = {
        let held = [0; 2].map(|_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap());
        held.map(|listener| listener.local_addr().unwrap().port())
    };
    let (third, alone) = (
        format!("127.0.0.1:{third_port}"),
        format!("127.0.0.1:{alone_port}"),
    );
    let (by_name, elsewhere) = (
        format!("lab-mini.invalid:{third_port}"),
        format!("lab-mini.invalid:{alone_port}"),
    );
    member::update_peers(&issuer.places.state, |book| {
        book.push(Peer {
            node: Some(node.clone()),
            key: Some(key),
            key_from: None,
            host: "lab-mini".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: third_port,
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
        });
        Ok(())
    })
    .unwrap();
    member.write_config(&format!(
        "{}\n[[fleet.peers]]\nhost = \"{third}\"\n\n[[fleet.peers]]\nhost = \"{by_name}\"\n\n\
         [[fleet.peers]]\nhost = \"{alone}\"\n\n[[fleet.peers]]\nhost = \"{elsewhere}\"\n",
        member.config()
    ));

    let grants = || fleet_table(&member.config()).unwrap().drive_from;
    let granted = vec![format!("{node}:{}", key.hex())];
    let words = [third.as_str(), "lab-mini", node.as_str(), by_name.as_str()];
    let grant: Vec<&str> = std::iter::once("drive")
        .chain(words.iter().flat_map(|&word| ["--from", word]))
        .collect();
    ok(allow(&member.places, &args(&grant)));
    assert_eq!(grants(), granted);
    let targets = known_peers(&member.places).unwrap_or_else(|f| panic!("{f:?}"));
    let known: Vec<Peer> = targets.peers.iter().map(|(peer, _)| peer.clone()).collect();
    for word in words {
        match bound_peer(&member.places, &known, word) {
            Ok(Bound::Node(peer)) => assert_eq!(
                (peer.node.as_deref(), peer.key.map(|key| key.hex())),
                (Some(node.as_str()), Some(key.hex())),
                "{word}"
            ),
            other => panic!("{word}: {other:?}"),
        }
    }
    let at_third: Vec<Option<&str>> = targets
        .peers
        .iter()
        .filter(|(peer, _)| peer.port == third_port)
        .map(|(peer, _)| peer.node.as_deref())
        .collect();
    assert_eq!(
        at_third,
        [None, None, Some(node.as_str())],
        "a read dials the person's entries and the copy beside them"
    );
    for word in [node.as_str(), by_name.as_str()] {
        let dialled = drive_fleet_in(&member.places, word, Duration::from_millis(100))
            .err()
            .expect("nothing listens there");
        assert!(
            dialled.why.starts_with(&format!("lab-mini:{third_port}: ")),
            "{word}: {dialled:?}"
        );
    }
    for word in [third.as_str(), "lab-mini", node.as_str()] {
        let ask = aterm_ctl::FleetAsk {
            host: Some(word),
            within: Duration::from_millis(100),
        };
        let read =
            survey_fleet_in(&member.places, &ask).unwrap_or_else(|why| panic!("{word}: {why}"));
        let labels: Vec<&str> = read.peers.iter().map(|peer| peer.label.as_str()).collect();
        assert_eq!(labels, [format!("lab-mini:{third_port}")], "{word}");
    }

    for word in [alone.as_str(), elsewhere.as_str()] {
        assert!(
            matches!(bound_peer(&member.places, &known, word), Ok(Bound::ByHand)),
            "{word}"
        );
        let refused = drive_fleet_in(&member.places, word, Duration::from_secs(1))
            .err()
            .expect("refused before any dial");
        assert!(refused.why.contains("named by hand"), "{word}: {refused:?}");
    }
}

// A state, each fleet by its role: A's, B's or C's, the subject's own, or one a verb minted.
#[derive(Clone, Debug, PartialEq, Eq)]
struct End {
    key: &'static str,
    code: &'static str,
    listen: bool,
    level: Option<&'static str>,
    grants: Vec<&'static str>,
    issuer: &'static str,
    members: usize,
    stale: usize,
    copy: &'static str,
}

#[derive(Clone, Copy, Debug)]
enum Fixture {
    // A member of A naming a peer in aterm.toml, with A's book copied.
    Member,
    Granted,
    // Granted, then a leave stopped short of aterm.toml.
    Left,
    // No state directory, nor aterm.toml.
    Empty,
    // No state directory (one removed by hand), and the `[fleet]` aterm.toml kept.
    Stale,
    // An issuer letting the member its book binds drive it.
    Issuer,
    // An issuer whose init was killed before aterm.toml.
    Unlistened,
}

#[derive(Clone, Copy, Debug)]
enum Other {
    Leave,
    // A leave stopped short of aterm.toml.
    StoppedLeave,
    // A join of C's code, which no verb joins.
    JoinC,
    // An init, or an init again where this machine issues a fleet.
    Init,
    // A kill, then the verb again.
    Kill,
    // The book's lock held across the verb's next write, then the verb again.
    Busy,
}

const DRIVER: &str = "d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4d4";

type Verb = fn(&Racing, &Places, Option<&FleetSecret>) -> Result<String, Failure>;

struct Race {
    verb: &'static str,
    fixture: Fixture,
    steps: &'static [&'static str],
    run: Verb,
    // The verb again after a kill or a busy lock, as its refusal says: `run` where none is named.
    again: Option<Verb>,
    alone: fn(&End) -> bool,
}

// Every writer of fleet state, with the steps another verb or a kill could come before.
const RACES: &[Race] = &[
    Race {
        verb: "join again",
        fixture: Fixture::Granted,
        steps: &["lock", "key", "drop", "config", "book"],
        run: |racing, places, _| join(places, &racing_opts(), &racing.a.1),
        again: None,
        alone: |end| end.grants == ["A"],
    },
    Race {
        verb: "first join",
        fixture: Fixture::Empty,
        steps: &["lock", "key", "drop", "config", "book"],
        run: |racing, places, _| join(places, &racing_opts(), &racing.a.1),
        again: None,
        alone: |end| end.key == "A" && end.issuer == "A",
    },
    Race {
        verb: "join of another code",
        fixture: Fixture::Granted,
        steps: &["lock", "key", "drop", "config", "book"],
        run: |racing, places, _| join(places, &racing_opts(), &racing.b.1),
        again: None,
        alone: |end| end.issuer == "B" && end.grants.is_empty(),
    },
    Race {
        verb: "allow drive",
        fixture: Fixture::Member,
        steps: &["grant"],
        run: |racing, places, _| allow(places, &args(&["drive", "--from", &racing.a_node()])),
        again: None,
        alone: |end| end.grants == ["A"],
    },
    Race {
        verb: "allow content",
        fixture: Fixture::Granted,
        steps: &["level"],
        run: |_, places, _| allow(places, &args(&["content"])),
        again: None,
        alone: |end| end.grants.is_empty(),
    },
    Race {
        verb: "peers",
        fixture: Fixture::Granted,
        steps: &["keep"],
        run: |_, places, _| peers(places, &[]),
        again: None,
        alone: |end| end.copy == "A",
    },
    Race {
        verb: "leave",
        fixture: Fixture::Granted,
        steps: &["removed", "unlisten"],
        run: |_, places, _| leave(places, &[]),
        again: None,
        alone: |end| end.key == "-" && !end.listen,
    },
    Race {
        verb: "leave of no state directory",
        fixture: Fixture::Stale,
        steps: &["removed", "unlisten"],
        run: |_, places, _| leave(places, &[]),
        again: None,
        alone: |end| end.key == "-" && !end.listen,
    },
    Race {
        verb: "first join beside a stale listen",
        fixture: Fixture::Stale,
        steps: &["lock", "key", "drop", "config", "book"],
        run: |racing, places, _| join(places, &racing_opts(), &racing.a.1),
        again: None,
        alone: |end| end.key == "A" && end.issuer == "A" && end.listen,
    },
    Race {
        verb: "init beside a stale listen",
        fixture: Fixture::Stale,
        steps: &["key", "code", "config"],
        run: |_, places, _| init(places, &racing_init()),
        again: Some(|_, places, _| init(places, &racing_init()).or_else(|_| init(places, &[]))),
        alone: |end| end.key == "minted" && end.listen,
    },
    Race {
        verb: "init",
        fixture: Fixture::Empty,
        steps: &["key", "code", "config"],
        run: |_, places, _| init(places, &racing_init()),
        again: Some(|_, places, _| init(places, &racing_init()).or_else(|_| init(places, &[]))),
        alone: |end| end.key == "minted" && end.listen,
    },
    Race {
        verb: "init after a stopped leave",
        fixture: Fixture::Left,
        steps: &["key", "code", "config"],
        run: |_, places, _| init(places, &racing_init()),
        again: Some(|_, places, _| init(places, &racing_init()).or_else(|_| init(places, &[]))),
        alone: |end| end.key == "minted" && end.listen && end.grants.is_empty(),
    },
    Race {
        verb: "init again",
        fixture: Fixture::Unlistened,
        steps: &["key"],
        run: |_, places, _| init(places, &[]),
        again: None,
        alone: |end| end.key == "own" && end.listen,
    },
    Race {
        verb: "rotate",
        fixture: Fixture::Issuer,
        steps: &["key", "code", "config"],
        run: |_, places, _| init(places, &args(&["--rotate"])),
        again: None,
        alone: |end| end.key == "minted" && end.grants == ["driver"],
    },
    Race {
        verb: "a join the issuer serves",
        fixture: Fixture::Issuer,
        steps: &["issuer"],
        run: serve_a_join,
        again: None,
        alone: |end| end.members == 2,
    },
];

fn racing_opts() -> Opts {
    Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    }
}

fn racing_init() -> Vec<String> {
    args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()])
}

fn granting_issuer(places: &Places) {
    ok(init(places, &racing_init()));
    let key = NodeIdentity::from_seed(DRIVER, &[5; 32]).unwrap().key();
    member::update_peers(&places.state, |book| {
        book.push(Peer {
            node: Some(DRIVER.to_owned()),
            key: Some(key),
            key_from: None,
            host: "127.0.0.1".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: closed_port(),
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
        });
        Ok(())
    })
    .unwrap();
    ok(allow(places, &args(&["drive", "--from", DRIVER])));
}

fn serve_a_join(
    _: &Racing,
    places: &Places,
    served: Option<&FleetSecret>,
) -> Result<String, Failure> {
    let reg = Registration {
        node: "e5".repeat(16),
        host: "lab-mini".to_owned(),
        port: 7199,
        version: "0.130.0".to_owned(),
        key: None,
    };
    let mut wire = Wire {
        input: std::io::Cursor::new(reg.line().into_bytes()),
        output: Vec::new(),
    };
    let state = places.state.clone();
    let issuer = move || {
        at_step("issuer");
        Ok(Issuer {
            node: member::node_id(&state).map_err(|e| e.to_string())?,
            host: "127.0.0.1".to_owned(),
            port: 7100,
            version: "0.95.0".to_owned(),
        })
    };
    let served = served.expect("the issuer's own key");
    match serve_join(
        &mut wire,
        (IpAddr::from(Ipv4Addr::LOCALHOST), None),
        (&places.state, served),
        issuer,
        7,
    ) {
        Ok(Ok(_)) => Ok(String::new()),
        Ok(Err(why)) => Err(failed(why)),
        Err(e) => Err(failed(e)),
    }
}

fn interfere(other: Other, c_code: &str, places: &Places) {
    match other {
        Other::Leave => drop(leave(places, &[])),
        Other::StoppedLeave => {
            let lock = places.config.with_file_name(".aterm.toml.aterm-write.lock");
            std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
            let _ = std::fs::remove_file(&lock);
            std::fs::create_dir(&lock).unwrap();
            drop(leave(places, &[]));
            std::fs::remove_dir(&lock).unwrap();
        }
        Other::JoinC => drop(join(places, &racing_opts(), c_code)),
        Other::Init => drop(init(places, &racing_init()).or_else(|_| init(places, &[]))),
        Other::Kill => panic!("killed here"),
        Other::Busy => unreachable!("the hook holds the book"),
    }
}

#[derive(Clone, Copy, Debug)]
enum When {
    Before,
    After,
    At(&'static str),
    Instead,
}

struct Racing {
    a: (World, String, Listener),
    b: (World, String, Listener),
    c: (World, String, Listener),
}

impl Racing {
    fn a_node(&self) -> String {
        member::node_id(&self.a.0.places.state).unwrap()
    }

    fn fixture(&self, fixture: Fixture) -> (World, Option<FleetSecret>) {
        for issuer in [&self.a.0, &self.b.0, &self.c.0] {
            member::update_peers(&issuer.places.state, |book| {
                book.retain(|peer| peer.via != Via::Joined);
                Ok(())
            })
            .unwrap();
        }
        let world = World::new("race");
        let places = &world.places;
        match fixture {
            Fixture::Empty => {}
            Fixture::Stale => world.write_config(&format!(
                "[fleet]\nlisten = \"0.0.0.0:{}\"\nallow = \"facts\"\n",
                closed_port()
            )),
            Fixture::Member | Fixture::Granted | Fixture::Left => {
                world.write_config(&format!(
                    "[[fleet.peers]]\nhost = \"127.0.0.1:{}\"\n",
                    closed_port()
                ));
                ok(join(places, &racing_opts(), &self.a.1));
                if !matches!(fixture, Fixture::Member) {
                    ok(allow(places, &args(&["drive", "--from", &self.a_node()])));
                }
                ok(peers(places, &[]));
                if matches!(fixture, Fixture::Left) {
                    interfere(Other::StoppedLeave, &self.c.1, places);
                }
            }
            Fixture::Issuer => granting_issuer(places),
            Fixture::Unlistened => {
                assert!(killed_at("config", || init(places, &racing_init())));
            }
        }
        let own = read_key_file(&places.state)
            .ok()
            .filter(|_| matches!(fixture, Fixture::Issuer | Fixture::Unlistened));
        (world, own)
    }

    fn end(&self, world: &World, own: Option<&FleetSecret>) -> End {
        let state = &world.places.state;
        let fleets: Vec<(&'static str, FleetSecret, String)> =
            [("A", &self.a.0), ("B", &self.b.0), ("C", &self.c.0)]
                .into_iter()
                .map(|(name, issuer)| {
                    let state = &issuer.places.state;
                    (
                        name,
                        read_key_file(state).unwrap(),
                        member::node_id(state).unwrap(),
                    )
                })
                .collect();
        let role = |secret: &FleetSecret| {
            fleets
                .iter()
                .find(|(_, key, _)| key == secret)
                .map_or(if Some(secret) == own { "own" } else { "minted" }, |f| f.0)
        };
        let node_role = |node: &str| {
            fleets
                .iter()
                .find(|(.., id)| id == node)
                .map_or(if node == DRIVER { "driver" } else { "?" }, |f| f.0)
        };
        let held = Held::read(state);
        let table = fleet_table(&world.config()).unwrap();
        let book = read_peers(state).unwrap_or_default();
        End {
            key: match &held {
                Held::Nothing => "-",
                Held::Unreadable => "unreadable",
                Held::Key(secret) => role(secret),
            },
            code: match member::read_join_code(state) {
                Err(_) => "-",
                Ok(code) => {
                    let secret = FleetSecret::new(&code.secret).unwrap();
                    if held.secret() == Some(&secret) {
                        "=key"
                    } else {
                        role(&secret)
                    }
                }
            },
            listen: table.listen.is_some(),
            level: table.level(),
            grants: table
                .drive_from
                .iter()
                .map(|word| node_role(&DriveGrant::node_of(word)))
                .collect(),
            issuer: book
                .iter()
                .find(|peer| peer.via == Via::Issuer)
                .and_then(|peer| peer.node.as_deref())
                .map_or("-", node_role),
            members: book.iter().filter(|peer| peer.via == Via::Joined).count(),
            stale: read_as_older(state).len() - book.len(),
            copy: match fleets
                .iter()
                .find(|(.., id)| member::holds_issuer_book(state, id))
            {
                Some(fleet) => fleet.0,
                None if state.join(ISSUER_BOOK_FILE).exists() => "?",
                None => "-",
            },
        }
    }

    // The end the verb leaves on a fresh fixture with `other` where `when` puts it, and whether
    // its last run did what it was asked.
    fn run(&self, race: &Race, with: Option<(Other, When)>) -> (End, bool) {
        let (world, own) = self.fixture(race.fixture);
        let places = &world.places;
        let own = own.as_ref();
        let c_code = &self.c.1;
        let verb = || (race.run)(self, places, own);
        let done = match with {
            None => verb().is_ok(),
            Some((other, When::Before)) => {
                interfere(other, c_code, places);
                verb().is_ok()
            }
            Some((other, When::After)) => {
                let done = verb().is_ok();
                interfere(other, c_code, places);
                done
            }
            Some((other, When::Instead)) => {
                interfere(other, c_code, places);
                false
            }
            Some((other, When::At(step))) => {
                let dir: &'static Path = Box::leak(places.state.clone().into_boxed_path());
                let here = Places {
                    state: places.state.clone(),
                    config: places.config.clone(),
                };
                let (reached, pending) = (Rc::new(Cell::new(false)), Rc::new(Cell::new(false)));
                let busy: Rc<RefCell<Option<member::HeldBook<'static>>>> = Rc::default();
                let hook = {
                    let (reached, pending, busy, c_code) = (
                        reached.clone(),
                        pending.clone(),
                        busy.clone(),
                        c_code.clone(),
                    );
                    move |now: &str| {
                        if now == step {
                            reached.set(true);
                            pending.set(true);
                        }
                        // A step under the book's lock: another verb waits it out and comes at
                        // the next step past it.
                        if pending.get()
                            && (matches!(other, Other::Kill) || !member::book_held(dir))
                        {
                            pending.set(false);
                            match other {
                                Other::Busy => {
                                    *busy.borrow_mut() = Some(member::hold_book(dir).unwrap())
                                }
                                other => interfere(other, &c_code, &here),
                            }
                        }
                    }
                };
                let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    with_step_hook(hook, verb)
                }));
                drop(busy.borrow_mut().take());
                assert!(reached.get(), "{} never reaches `{step}`", race.verb);
                if pending.get() && !matches!(other, Other::Busy) {
                    interfere(other, c_code, places);
                }
                matches!(ran, Ok(Ok(_)))
                    || matches!(other, Other::Kill | Other::Busy)
                        && race.again.unwrap_or(race.run)(self, places, own).is_ok()
            }
        };
        (self.end(&world, own), done)
    }
}

fn race(racing: &Racing, race: &Race) -> Vec<String> {
    let verb = race.verb;
    let (alone, done) = racing.run(race, None);
    if !done || !(race.alone)(&alone) {
        return vec![format!("{verb} alone: {alone:#?}")];
    }
    let mut unserial = Vec::new();
    for other in [
        Other::Leave,
        Other::StoppedLeave,
        Other::JoinC,
        Other::Init,
        Other::Kill,
        Other::Busy,
    ] {
        let (serial, instead) = if matches!(other, Other::Kill | Other::Busy) {
            (vec![alone.clone()], None)
        } else {
            (
                vec![
                    racing.run(race, Some((other, When::Before))).0,
                    racing.run(race, Some((other, When::After))).0,
                ],
                Some(racing.run(race, Some((other, When::Instead))).0),
            )
        };
        for &step in race.steps {
            let (end, done) = racing.run(race, Some((other, When::At(step))));
            // A verb refused once the other came between leaves what the other makes alone.
            let want: Vec<&End> = serial
                .iter()
                .chain(instead.iter().filter(|_| !done))
                .collect();
            if !want.contains(&&end) {
                unserial.push(format!(
                    "{verb}, {other:?} at `{step}`, ends as no serial order does:\n{end:#?}\nwhere \
                     one of these:\n{want:#?}"
                ));
            }
        }
    }
    unserial
}

#[test]
fn every_fleet_writer_ends_as_some_serial_order_would() {
    let unserial: Vec<String> = std::thread::scope(|scope| {
        let races: Vec<_> = RACES
            .iter()
            .enumerate()
            .map(|(at, one)| {
                scope.spawn(move || {
                    let racing = Racing {
                        a: issuing(&format!("race{at}-a")),
                        b: issuing(&format!("race{at}-b")),
                        c: issuing(&format!("race{at}-c")),
                    };
                    race(&racing, one)
                })
            })
            .collect();
        races
            .into_iter()
            .flat_map(|one| one.join().unwrap())
            .collect()
    });
    assert!(unserial.is_empty(), "{}", unserial.join("\n"));
}

#[test]
fn a_rotate_after_a_leave_stopped_past_its_key_is_refused_and_an_init_keeps_no_grant() {
    let world = World::new("rotate-keyless");
    let places = &world.places;
    granting_issuer(places);
    // A leave stopped past its first removal, the key's, leaves the code beside no key.
    std::fs::remove_file(places.state.join(KEY_FILE)).unwrap();
    let config = world.config();
    let refused = init(places, &args(&["--rotate"]));
    assert!(
        matches!(&refused, Err(Failure::Usage(why)) if why.contains("it issues none")),
        "{refused:?}"
    );
    assert_eq!(
        (world.config(), Held::read(&places.state)),
        (config, Held::Nothing)
    );
    let inited = ok(init(places, &racing_init()));
    assert!(inited.contains(GRANTS_GONE), "{inited}");
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from.len()),
        (Some(FleetScope::Content.word()), 0)
    );
}

#[test]
fn an_init_again_beside_a_code_its_key_is_not_refuses_and_a_rotate_again_mints_both() {
    let world = World::new("rotate-split");
    let places = &world.places;
    granting_issuer(places);
    let key = read_key_file(&places.state).unwrap();
    assert!(killed_at("code", || init(places, &args(&["--rotate"]))));
    let code = std::fs::read_to_string(places.state.join(CODE_FILE)).unwrap();
    let refused = init(places, &[]);
    assert!(
        matches!(&refused, Err(Failure::Failed(why)) if why.contains("`aterm fleet init --rotate` again mints both")),
        "{refused:?}"
    );
    assert_eq!(
        (
            read_key_file(&places.state).unwrap(),
            std::fs::read_to_string(places.state.join(CODE_FILE)).unwrap()
        ),
        (key.clone(), code),
        "nothing written"
    );
    ok(init(places, &args(&["--rotate"])));
    let rotated = read_key_file(&places.state).unwrap();
    assert_ne!(rotated, key);
    assert_eq!(
        FleetSecret::new(&member::read_join_code(&places.state).unwrap().secret).unwrap(),
        rotated
    );
    assert_eq!(fleet_table(&world.config()).unwrap().drive_from.len(), 1);
}

#[test]
fn a_grant_beside_a_key_missing_or_unread_is_refused_saying_which() {
    let world = World::new("keyless-grant");
    let places = &world.places;
    granting_issuer(places);
    ok(allow(places, &args(&["content"])));
    let config = world.config();
    let key = places.state.join(KEY_FILE);
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let unread = allow(places, &args(&["drive", "--from", DRIVER]));
    assert!(
        matches!(&unread, Err(Failure::Failed(why)) if why.contains("is not private") && why.contains("(0600, one link)") && !why.contains("leave")),
        "{unread:?}"
    );
    assert_eq!(world.config(), config, "nothing granted");
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    // A leave stopped past its first removal, the key's, then past its second, the code's.
    for gone in [KEY_FILE, CODE_FILE] {
        std::fs::remove_file(places.state.join(gone)).unwrap();
        let refused = allow(places, &args(&["drive", "--from", DRIVER]));
        assert!(
            matches!(&refused, Err(Failure::Failed(why)) if why.contains("holds no fleet's key")),
            "{refused:?}"
        );
        assert_eq!(world.config(), config, "nothing granted");
    }
}

#[test]
fn an_init_again_of_one_stopped_before_aterm_toml_writes_what_it_asked() {
    let world = World::new("init-asked-again");
    let places = &world.places;
    let port = closed_port();
    let asked = args(&[
        "--host",
        "127.0.0.1",
        "--port",
        &port.to_string(),
        "--allow",
        "content",
    ]);
    assert!(killed_at("config", || init(places, &asked)));
    let config = world.config();
    let refused = init(places, &args(&["--port", &(port ^ 1).to_string()]));
    assert!(
        matches!(&refused, Err(Failure::Usage(why)) if why.contains("takes `--rotate`")),
        "{refused:?}"
    );
    assert_eq!(world.config(), config, "nothing written");
    let again = ok(init(places, &asked));
    assert!(again.contains("already the fleet's issuer"), "{again}");
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(
        (table.listen_port(), table.level()),
        (Some(port), Some(FleetScope::Content.word()))
    );
    let whole = init(places, &asked);
    assert!(
        matches!(&whole, Err(Failure::Usage(why)) if why.contains("changes nothing without") && why.contains("`aterm fleet allow content`")),
        "{whole:?}"
    );
}
