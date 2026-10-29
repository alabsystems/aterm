// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for [`super`].

use super::*;
use aterm_net::fleet::{FleetEvent, Listening, read_key_file, serve_with_handshake};
use aterm_net::fleet_member::{Issuer, read_peers, serve_book, serve_join};
use aterm_uds::private_file::{remove_private, write_private};
use std::cell::{Cell, RefCell};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const HANG: Duration = Duration::from_secs(60);

#[test]
fn every_cli_report_ends_with_a_newline() {
    assert_eq!(
        with_final_newline("no fleet code".into()),
        "no fleet code\n"
    );
    assert_eq!(with_final_newline("already\n".into()), "already\n");
}

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
        hint: None,
    }
    .text_at(("m7.local", 7100))
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
fn an_init_or_a_join_names_no_host_a_fleet_never_dials() {
    let world = World::new("init-public-host");
    for (unheard, said) in [
        ("8.8.8.8", "is no LAN address"),
        ("100.64.0.1", "is no LAN address"),
        ("2001:4860::8888", "is no LAN address"),
        ("fd00::5", "is an IPv6 address"),
        ("fe80::1", "is an IPv6 address"),
        ("::ffff:10.0.0.8", "is an IPv6 address"),
    ] {
        for verb in [&["--host", unheard][..], &["--rotate", "--host", unheard]] {
            let refused = match init(&world.places, &args(verb)) {
                Err(Failure::Usage(why)) => why,
                other => panic!("{verb:?}: {other:?}"),
            };
            assert!(
                refused.contains(said) && refused.ends_with("nothing was written"),
                "{verb:?}: {refused}"
            );
        }
        assert!(matches!(
            parse_join(&args(&["--host", unheard])),
            Err(Failure::Usage(_))
        ));
    }
    assert!(!world.places.state.exists(), "nothing was written");
    assert!(world.config().is_empty());
    for lan in [
        "127.0.0.1",
        "::1",
        "10.0.0.7",
        "192.168.1.4",
        "169.254.3.3",
        "m7.local",
    ] {
        let opts = parse_join(&args(&["--host", lan])).expect(lan);
        assert_eq!(opts.host.as_deref(), Some(lan));
    }
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
            machine: None,
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

/// A fleet listener served in process, which so must finish each handshake: it gives each one
/// [`HANG`] ([`serve_with_handshake`]).
struct Listener {
    running: Arc<AtomicBool>,
    round: Arc<AtomicBool>,
    serving: Option<std::thread::JoinHandle<TcpListener>>,
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
        Self::naming(listener, state, serves, (None, Vec::new()))
    }

    fn claiming(listener: TcpListener, state: &Path, claim: &str) -> Self {
        Self::naming(
            listener,
            state,
            &[FleetOp::Survey, FleetOp::Book, FleetOp::Join],
            (Some(claim.to_owned()), Vec::new()),
        )
    }

    fn with_rows(listener: TcpListener, state: &Path, rows: Vec<String>) -> Self {
        Self::naming(
            listener,
            state,
            &[FleetOp::Survey, FleetOp::Book, FleetOp::Join],
            (None, rows),
        )
    }

    fn rereading(listener: TcpListener, state: &Path) -> Self {
        Self::reading(
            listener,
            state,
            &[FleetOp::Survey, FleetOp::Book, FleetOp::Join],
            (None, Vec::new()),
            true,
        )
    }

    fn naming(
        listener: TcpListener,
        state: &Path,
        serves: &'static [FleetOp],
        named: (Option<String>, Vec<String>),
    ) -> Self {
        Self::reading(listener, state, serves, named, false)
    }

    fn reading(
        listener: TcpListener,
        state: &Path,
        serves: &'static [FleetOp],
        (claim, rows): (Option<String>, Vec<String>),
        rereads: bool,
    ) -> Self {
        let _ = aterm_uds::keep_out_of_children(&listener);
        let port = listener.local_addr().unwrap().port();
        let running = Arc::new(AtomicBool::new(true));
        let round = Arc::new(AtomicBool::new(true));
        let node = member::read_node_identity(state).expect("a node key it can read");
        let issuer = Arc::new(Issuer {
            node: member::node_id(state).unwrap(),
            host: "127.0.0.1".to_owned(),
            port,
            version: "0.95.0".to_owned(),
        });
        // A fixed-key listener represents the key at construction, even when its
        // worker starts after the caller rotates the key. Later rounds may reread it, and keep
        // the one they serve while none is on disk.
        let mut secret = read_key_file(state).expect("init wrote the key");
        let mut first = true;
        let state = state.to_path_buf();
        let serving = std::thread::spawn({
            let (running, round) = (Arc::clone(&running), Arc::clone(&round));
            move || loop {
                round.store(true, Ordering::Release);
                if !running.load(Ordering::Acquire) {
                    break listener;
                }
                if !std::mem::take(&mut first)
                    && let Ok(read) = read_key_file(&state)
                {
                    secret = read;
                }
                let listening = Listening {
                    secret: secret.clone(),
                    scope: FleetScope::Facts,
                    node: node.clone(),
                    serves,
                    slots: 8,
                    per_source: 8,
                    lifetime: HANG,
                };
                let (issuer, claim, rows) = (Arc::clone(&issuer), claim.clone(), rows.clone());
                let (state, secret) = (state.clone(), listening.secret.clone());
                let refused = Arc::clone(&round);
                serve_with_handshake(
                    &listener,
                    &listening,
                    &round,
                    HANG,
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
                                    "OK {}\ninstance pid=1 host=127.0.0.1 version=0.95.0 \
                                     sessions={} level=facts cut=0 at_ms=1 node={}\n{}",
                                    rows.len() + 1,
                                    rows.len(),
                                    claim.as_deref().unwrap_or(&issuer.node),
                                    rows.iter()
                                        .map(|row| format!("{row}\n"))
                                        .collect::<String>()
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
                    move |event: FleetEvent| {
                        if rereads && matches!(event, FleetEvent::Refused { .. }) {
                            refused.store(false, Ordering::Release);
                        }
                    },
                );
                if !rereads {
                    break listener;
                }
            }
        });
        Self {
            running,
            round,
            serving: Some(serving),
        }
    }

    fn take(mut self) -> TcpListener {
        self.stop().expect("a listener serving")
    }

    fn stop(&mut self) -> Option<TcpListener> {
        self.running.store(false, Ordering::Release);
        self.round.store(false, Ordering::Release);
        self.serving.take()?.join().ok()
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn closed_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

/// A host word with no port that nothing here names is dialled at the default port.
#[test]
fn a_survey_of_a_host_alone_dials_it_at_the_default_port() {
    let world = World::new("bare-host");
    let port = closed_port().to_string();
    ok(init(
        &world.places,
        &args(&["--host", "127.0.0.1", "--port", &port]),
    ));
    let ask = aterm_ctl::FleetAsk {
        host: Some("127.0.0.1"),
        within: HANG,
    };
    let read = survey_fleet_in(&world.places, &ask).unwrap_or_else(|why| panic!("{why}"));
    let at = format!("nothing listens on 127.0.0.1:{}", member::default_port());
    assert!(
        matches!(&read.peers[..], [one] if one.answer.as_ref().is_err_and(|why| why.contains(&at))),
        "{at}: {read:?}"
    );
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
    let again = ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port, "--allow", "content"]),
    ));
    assert!(again.contains("already the fleet's issuer"), "{again}");
    let drive = init(&issuer.places, &args(&["--allow", "drive"]));
    assert!(
        matches!(&drive, Err(Failure::Usage(why)) if why.contains("never by an init or a join")),
        "{drive:?}"
    );
    assert_eq!(read_key_file(&issuer.places.state).unwrap(), key);
    assert_eq!(std::fs::read_to_string(&code_path).unwrap(), code);
    assert_eq!(fleet_table(&issuer.config()).unwrap(), table);

    let _serving = Listener::serve(listener, &issuer.places.state);
    let vault = vault_code(&issuer);
    let member = World::new("member");
    let member_port = closed_port();
    let loopback = || Some("127.0.0.1".to_owned());
    let opts = Opts {
        port: Some(member_port),
        host: loopback(),
        ..Opts::default()
    };
    let joined = ok(join(&member.places, &opts, &vault));
    assert!(
        joined.contains("allow      facts:")
            && joined.contains("`aterm fleet allow content` lets it")
            && joined.contains("(1 peer in its book)"),
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
    ok(join(&member.places, &again, &vault));
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
    let other_code = vault_code(&elsewhere);
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
    let elsewhere_port = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    assert!(matches!(
        join(&fresh.places, &elsewhere_port, &other_code),
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

/// AWAY FROM EVERY MEMBER (docs/DESIGN-fluid-swarm-2026-10-28.md F4): when every peer `peers`
/// dialled went unanswered, the listing ends by saying so once, with the last this book heard of
/// one; where a peer answered, it does not.
/// NEGATIVE CONTROL: a listing that never says the away line fails the issuer's.
#[test]
fn a_peers_listing_that_reached_nobody_says_away_and_when_the_book_last_heard_of_one() {
    let (issuer, code, _serving) = issuing("away-issuer");
    let member = World::new("away-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let heard = read_peers(&issuer.places.state).unwrap()[0]
        .seen_ms
        .expect("the join is heard");
    let at = i64::try_from(heard).unwrap();
    let offset = aterm_uds::zone::local_offset_at(at / 1000).unwrap_or(0);
    let clock = crate::supervise::occupancy::said::clock_words(at, offset);
    let listed = ok(list_peers(&issuer.places, HANG));
    assert!(
        listed.contains(&format!(
            "Away from every member: the one peer dialled did not answer; the last this book \
             heard of one was at {clock}"
        )),
        "the member runs no aterm:\n{listed}"
    );
    let theirs = ok(list_peers(&member.places, HANG));
    assert!(
        !theirs.contains("Away from every member"),
        "the issuer answered:\n{theirs}"
    );
}

/// AWAY SINCE THE LAST ANSWER (docs/DESIGN-fluid-swarm-2026-10-28.md, F4's follow-up): a member
/// whose `peers` its issuer answered records when, by this Mac's clock; once the issuer answers
/// no more, the member's listing says it is away from every member since that answer, and a line
/// of its own after the table says when the issuer last answered, while the issuer's row reads
/// as it did (its CHECK words alone), for a script that reads the rows.
/// The answer is set back 90 minutes before the listing, so it stands apart from the book's
/// `seen_ms` for the issuer (the join's time, moments before the first answer), and the "since"
/// and the issuer's line must say the answer's time.
/// NEGATIVE CONTROL: a `keep_book` that notes no answer fails "the issuer's answer is recorded"
/// (and, past it, the listing has no "since" line and the issuer's line reads "none recorded");
/// the issuer's row ending in when it last answered fails "the issuer's row reads as it did";
/// `peers` taking the times from the book's `seen_ms` in place of the answers fails "since the
/// issuer's last answer" (it says the join's clock, 0s ago).
#[test]
fn a_member_away_from_its_issuer_says_since_the_issuers_last_answer() {
    let (issuer, code, serving) = issuing("since-issuer");
    let member = World::new("since-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let answering = ok(list_peers(&member.places, HANG));
    assert!(
        !answering.contains("Away from every member"),
        "the issuer answered:\n{answering}"
    );
    let answered = member::read_answered(&member.places.state);
    let at = *answered
        .get(&format!("node={issuer_node}"))
        .unwrap_or_else(|| panic!("the issuer's answer is recorded: {answered:?}\n{answering}"));
    // The answer 90 minutes back, apart from the book's time for the issuer (a note keeps the
    // later time, so the file is written over).
    let back = at - 90 * 60_000;
    std::fs::write(
        member.places.state.join(member::ANSWERED_FILE),
        format!("answered node={issuer_node} ms={back}\n"),
    )
    .unwrap();
    // The issuer's port refuses from here on: the member is away from its one member.
    drop(serving.take());
    let away = ok(list_peers(&member.places, HANG));
    let back_ms = i64::try_from(back).unwrap();
    let offset = aterm_uds::zone::local_offset_at(i64::try_from(at).unwrap() / 1000);
    let clock = format!(
        "{}{} (1h ago)",
        crate::supervise::occupancy::said::clock_words(back_ms, offset.unwrap_or(0)),
        if offset.is_some() { "" } else { " UTC" }
    );
    assert!(
        away.contains("Away from every member: the one peer dialled did not answer"),
        "the first line as F4 wrote it:\n{away}"
    );
    assert!(
        away.contains(&format!(
            "\nAway from every member since {clock}: the last answer one gave this Mac.\n"
        )),
        "since the issuer's last answer:\n{away}"
    );
    let row = away
        .lines()
        .find(|line| line.starts_with("  ") && line.contains(&issuer_node[..8]))
        .unwrap_or_else(|| panic!("the issuer's row:\n{away}"));
    assert!(
        !row.contains("answered") && !row.contains("recorded"),
        "the issuer's row reads as it did, its CHECK words alone:\n{away}"
    );
    assert!(
        away.contains(&format!(
            "\nLast answer to this Mac from {}: {clock}.\n",
            &issuer_node[..8]
        )),
        "a line of its own says when the issuer last answered:\n{away}"
    );
}

/// Each member that did not answer gets a line of its own after the table, in the table's order:
/// a peer with no node id by its place, one this Mac holds no answer of as "none recorded".
/// NEGATIVE CONTROL: a line per member that leaves out the clock fails the first line.
#[test]
fn each_member_that_did_not_answer_gets_a_line_of_its_own_after_the_table() {
    let now = 10 * 3_600_000;
    assert_eq!(
        last_answer_words(
            &[
                ("1a2b3c4d".to_owned(), Some(now - 5_400_000)),
                ("10.0.0.9:7100".to_owned(), None),
            ],
            now,
            Some(3_600),
        ),
        "Last answer to this Mac from 1a2b3c4d: 09:30 (1h ago).\n\
         Last answer to this Mac from 10.0.0.9:7100: none recorded.\n"
    );
    assert_eq!(last_answer_words(&[], now, None), "", "everyone answered");
}

/// The away line's words: said only when peers were dialled and none answered, with the newest
/// time the book heard of one as a local clock (UTC when the zone is unknown) and an age; then,
/// where this Mac recorded one, since when, by the last answer a member gave this Mac (F4's
/// follow-up), on a line of its own after it, so the first line reads as it did.
/// NEGATIVE CONTROL: an away line despite a peer that answered fails "one answered"; a "since"
/// said by the book's `seen_ms` (a registration's time, not an answer) fails "since the last
/// answer, not the book's time": it reads 09:00 (2h ago), not 09:30 (1h ago).
#[test]
fn away_is_said_only_when_every_peer_dialled_went_unanswered() {
    let now = 10 * 3_600_000;
    assert_eq!(
        away_words((1, 30), (Some(0), Some(0)), now, Some(0)),
        None,
        "one answered"
    );
    assert_eq!(
        away_words((0, 0), (Some(0), Some(0)), now, Some(0)),
        None,
        "none dialled"
    );
    assert_eq!(
        away_words((0, 31), (Some(now - 7_200_000), None), now, Some(3_600)).as_deref(),
        Some(
            "Away from every member: none of the 31 peers dialled answered; the last this book \
             heard of one was at 09:00 (2h ago).\n"
        )
    );
    assert_eq!(
        away_words(
            (0, 31),
            (Some(now - 7_200_000), Some(now - 5_400_000)),
            now,
            Some(3_600)
        )
        .as_deref(),
        Some(
            "Away from every member: none of the 31 peers dialled answered; the last this book \
             heard of one was at 09:00 (2h ago).\n\
             Away from every member since 09:30 (1h ago): the last answer one gave this Mac.\n"
        ),
        "since the last answer, not the book's time"
    );
    assert_eq!(
        away_words((0, 1), (Some(now - 60_000), None), now, None).as_deref(),
        Some(
            "Away from every member: the one peer dialled did not answer; the last this book \
             heard of one was at 09:59 UTC (1m ago).\n"
        )
    );
    assert_eq!(
        away_words((0, 2), (None, Some(now - 120_000)), now, None).as_deref(),
        Some(
            "Away from every member: none of the 2 peers dialled answered; this book holds no \
             time it heard of one.\n\
             Away from every member since 09:58 UTC (2m ago): the last answer one gave this Mac.\n"
        )
    );
    assert_eq!(
        away_words((0, 2), (None, None), now, Some(0)).as_deref(),
        Some(
            "Away from every member: none of the 2 peers dialled answered; this book holds no \
             time it heard of one.\n"
        )
    );
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
    let code = vault_code(&issuer);
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
        refused.contains(&format!("`aterm fleet peers rm {node}` on the issuer")),
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

    let removed = ok(peers(&issuer.places, &args(&["rm", short(&node)])));
    assert!(
        removed.contains(
            "it may keep the fleet's secret (it does unless it left), and while it does it reads \
             this fleet's surveys: only `aterm fleet init --rotate`"
        ),
        "{removed}"
    );
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
fn a_join_again_of_the_same_fleet_is_answered_by_any_member_that_serves_it() {
    let (issuer, code, serving) = issuing("any-issuer");
    let member = World::new("any-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let node = member::node_id(&member.places.state).unwrap();
    // Another member: its own node, under the fleet's secret.
    let secret = JoinCode::parse(&code).unwrap().secret;
    let other = World::new("any-other");
    ok(init(
        &other.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    member::hold_book(&other.places.state)
        .unwrap()
        .write_key(&secret)
        .unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let there = JoinCode { secret, hint: None }
        .text_at(("127.0.0.1", listener.local_addr().unwrap().port()));
    let _serving_other = Listener::serve(listener, &other.places.state);
    drop(serving);
    let answered = |world: &World| {
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
    assert!(answered(&issuer));
    ok(join(&member.places, &opts, &there));
    assert!(
        answered(&other),
        "with the issuer dark, the member that answered recorded it"
    );
    assert!(
        read_peers(&other.places.state)
            .unwrap()
            .iter()
            .any(|peer| peer.node.as_deref() == Some(node.as_str())),
        "that member's book holds this one"
    );
    assert_eq!(
        read_key_file(&member.places.state).unwrap(),
        FleetSecret::new(&secret).unwrap()
    );
}

/// A new Mac whose code names a member that is away joins through another member its
/// `_alab._tcp` advert names (F17), once none of the places it knows answered.
/// NEGATIVE CONTROL: a join that reads no adverts reaches nobody and is refused.
#[test]
fn a_new_mac_joins_through_an_advertised_member_while_the_code_names_one_away() {
    let (_issuer, code, serving) = issuing("advert-issuer");
    let secret = JoinCode::parse(&code).unwrap().secret;
    let other = World::new("advert-other");
    ok(init(
        &other.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    member::hold_book(&other.places.state)
        .unwrap()
        .write_key(&secret)
        .unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let advertised = listener.local_addr().unwrap().port();
    let _serving_other = Listener::serve(listener, &other.places.state);
    // The member the code names is away.
    drop(serving);
    let bell = Arc::new(aterm_secrets::wake::Bell::default());
    let adverts = aterm_secrets::discover::Watch::idle_resolving(
        crate::node_service::ALAB_TCP_SERVICE,
        &bell,
        &[],
        move |instance| {
            (instance == "alab-other").then(|| aterm_secrets::discover::Advert {
                instance: instance.to_owned(),
                host: "127.0.0.1".to_owned(),
                port: advertised,
                txt: Vec::new(),
            })
        },
    );
    adverts.feed(
        "16:02:11.123  Add        3   1 local.               _alab._tcp.          alab-other\n",
    );
    let newcomer = World::new("advert-new");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        adverts: Some(Arc::new(adverts)),
        ..Opts::default()
    };
    ok(join(&newcomer.places, &opts, &code));
    let node = member::node_id(&newcomer.places.state).unwrap();
    assert!(
        read_peers(&other.places.state)
            .unwrap()
            .iter()
            .any(|peer| peer.node.as_deref() == Some(node.as_str())),
        "the advertised member recorded the new Mac"
    );
    assert_eq!(
        read_key_file(&newcomer.places.state).unwrap(),
        FleetSecret::new(&secret).unwrap()
    );
}

/// A join whose first place is an aterm before 0.147 that holds no join code, and so records no
/// member ([`member::NOT_THE_ISSUER`]), goes on to the next place, a member that records it.
/// NEGATIVE CONTROL: end the join at that refusal, as at a member's own words: the join is
/// refused.
#[test]
fn a_join_passes_over_an_older_member_that_records_none() {
    use std::io::{Read as _, Write as _};
    let (_issuer, code, _serving) = issuing("older-issuer");
    let secret = JoinCode::parse(&code).unwrap().secret;
    let older = World::new("older-member");
    ok(init(
        &older.places,
        &args(&["--host", "127.0.0.1", "--port", &closed_port().to_string()]),
    ));
    member::hold_book(&older.places.state)
        .unwrap()
        .write_key(&secret)
        .unwrap();
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let older_port = listener.local_addr().unwrap().port();
    let listening = Listening {
        secret: FleetSecret::new(&secret).unwrap(),
        scope: FleetScope::Facts,
        node: member::read_node_identity(&older.places.state).unwrap(),
        serves: &[FleetOp::Join],
        slots: 4,
        per_source: 4,
        lifetime: HANG,
    };
    let running = Arc::new(AtomicBool::new(true));
    let serving_older = std::thread::spawn({
        let running = Arc::clone(&running);
        move || {
            serve_with_handshake(
                &listener,
                &listening,
                &running,
                HANG,
                |served, _| {
                    served.answer(|stream| {
                        stream.set_read_timeout(Some(HANG))?;
                        let mut byte = [0u8; 1];
                        while stream.read(&mut byte)? == 1 && byte[0] != b'\n' {}
                        stream.write_all(format!("ERR {}\n", member::NOT_THE_ISSUER).as_bytes())
                    })
                },
                |_| {},
            );
        }
    });
    let newcomer = World::new("older-new");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        at: Some(("127.0.0.1".to_owned(), older_port)),
        ..Opts::default()
    };
    let joined = join(&newcomer.places, &opts, &code);
    running.store(false, Ordering::Release);
    let _ = std::net::TcpStream::connect((Ipv4Addr::LOCALHOST, older_port));
    let _ = serving_older.join();
    ok(joined);
    assert_eq!(
        read_key_file(&newcomer.places.state).unwrap(),
        FleetSecret::new(&secret).unwrap()
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
        hint: None,
    }
    .text_at(("127.0.0.1", closed_port()));
    let other = refused(&other);
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
            && none.contains(&narrowed)
            && !none.contains("  off "),
        "{none}"
    );
    assert!(!state.join(member::OFF_FILE).exists(), "it wrote nothing");
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
        refused.starts_with("fleet.key is open to other users (")
            && refused.contains("cannot tell whether the code is the fleet")
            && refused.contains(
                "nothing of the fleet was written here: `chmod 600` it, then run this again, or \
                 `aterm fleet leave` and join again"
            )
            && !refused.contains("joined this fleet before"),
        "{refused}"
    );
    assert_eq!(
        read_peers(&other.places.state).unwrap(),
        Vec::new(),
        "nothing was sent it"
    );
    write_private(&member.places.state, KEY_FILE, b"not a secret\n").unwrap();
    let malformed = match join(&member.places, &opts, &other_code) {
        Err(Failure::Failed(why)) => why,
        result => panic!("{result:?} should be refused"),
    };
    assert!(
        malformed.starts_with("fleet.key holds no fleet secret: ")
            && malformed.ends_with(
                "nothing of the fleet was written here: `aterm fleet leave`, join again"
            ),
        "one remedy, named once: {malformed}"
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
        let at = JoinCode { secret, hint: None }
            .text_at(("127.0.0.1", listener.local_addr().unwrap().port()));
        (Listener::serve(listener, &issuer.places.state), at)
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
        refused.contains("the issuer lost its node key")
            && refused.contains(&format!("`aterm fleet peers rm {node}` here")),
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
        refused.contains(&format!("`aterm fleet peers rm {node}` here")),
        "{refused}"
    );
    ok(peers(&member.places, &args(&["rm", &node])));
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
    let code = vault_code(&issuer);
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
                signed: None,
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
                signed: None,
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
        fleet_table(&issuer.config()).unwrap().listen,
        Some(format!("0.0.0.0:{}", member::default_port())),
        "the port stays"
    );
    let code_of = |world: &World| {
        let text = std::fs::read_to_string(world.places.state.join(CODE_FILE)).unwrap();
        let listen = fleet_table(&world.config()).unwrap().listen.unwrap();
        let hint = JoinCode::parse(&text).unwrap().hint;
        assert!(
            hint.as_ref()
                .is_some_and(|(_, port)| listen.ends_with(&format!(":{port}"))),
            "the code's file names where its minter listens, for an aterm before 0.147: {hint:?}"
        );
        listen
    };
    assert_eq!(
        code_of(&issuer),
        format!("0.0.0.0:{}", member::default_port()),
        "the port stays"
    );

    remove_private(&issuer.places.state, CODE_FILE).unwrap();
    assert!(matches!(init(&issuer.places, &[]), Err(Failure::Failed(_))));

    let looped = World::new("rotate-loopback");
    ok(init(
        &looped.places,
        &args(&["--host", "127.0.0.1", "--port", "17320"]),
    ));
    ok(init(&looped.places, &args(&["--rotate"])));
    assert_eq!(code_of(&looped), "127.0.0.1:17320");
    assert_eq!(
        fleet_table(&looped.config()).unwrap().listen.as_deref(),
        Some("127.0.0.1:17320")
    );
    ok(init(
        &looped.places,
        &args(&["--rotate", "--port", "17321"]),
    ));
    assert_eq!(code_of(&looped), "127.0.0.1:17321");

    let fresh = World::new("rotate-fresh");
    assert!(matches!(
        init(&fresh.places, &args(&["--rotate"])),
        Err(Failure::Usage(_))
    ));
    assert!(!fresh.places.state.join(KEY_FILE).exists());
}

#[test]
fn a_rotate_of_an_open_state_keeps_the_place_it_listens_at() {
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
    assert_eq!(
        code.hint,
        Some(("127.0.0.1".to_owned(), 17322)),
        "the code's file names where its minter listens, for an aterm before 0.147"
    );
    assert_eq!(
        fleet_table(&issuer.config()).unwrap().listen.as_deref(),
        Some("127.0.0.1:17322")
    );
}

#[test]
fn a_rotate_returns_once_this_machines_listener_serves_the_new_key() {
    let (issuer, _, old) = issuing("rotate-serves");
    let port = fleet_table(&issuer.config())
        .unwrap()
        .listen_port()
        .unwrap();
    let held = old.take();
    let serving = Listener::rereading(held, &issuer.places.state);
    let rotated = ok(init(&issuer.places, &args(&["--rotate"])));
    assert!(!rotated.contains("  listener "), "{rotated}");
    assert!(rotated.contains(FOLLOWS), "{rotated}");
    let code = vault_code(&issuer);
    let member = World::new("rotate-serves-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let stuck = Listener::serve(serving.take(), &issuer.places.state);
    let again = ok(init(&issuer.places, &args(&["--rotate"])));
    assert!(
        again.contains(&format!(
            "  listener   127.0.0.1:{port} still answers under another secret 3 s after the new \
             key was written"
        )),
        "{again}"
    );
    assert!(
        !again.contains(FOLLOWS)
            && again.contains(&format!("`--at 127.0.0.1:{port}` names this one.\n")),
        "no close that says it listens: {again}"
    );
    drop(stuck);
}

#[test]
fn a_new_fleets_init_returns_once_this_machines_listener_serves_its_key() {
    let (issuer, _, old) = issuing("init-serves");
    let port = fleet_table(&issuer.config())
        .unwrap()
        .listen_port()
        .unwrap();
    let serving = Listener::rereading(old.take(), &issuer.places.state);
    ok(leave(&issuer.places, &[]));
    let fresh = ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    assert!(!fresh.contains("  listener "), "{fresh}");
    let code = vault_code(&issuer);
    let member = World::new("init-serves-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    drop(serving);
}

/// `act` with each verb's read of the issuer's book bounded by [`HANG`] ([`issuer_read_within`]),
/// for an issuer that serves, and so must answer: a [`Listener`] gives its side of each handshake
/// the same, so the read waits on nothing but the hang detector.
fn with_issuer_serving<R>(act: impl FnOnce() -> R) -> R {
    with_issuer_read_within(HANG, act)
}

/// `act` with each verb's read of the issuer's book bounded by `within` ([`issuer_read_within`]).
fn with_issuer_read_within<R>(within: Duration, act: impl FnOnce() -> R) -> R {
    ISSUER_READ_WITHIN.set(Some(within));
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(act));
    ISSUER_READ_WITHIN.set(None);
    done.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

/// `act` with each `peers` listing's, leave's and join's read of the fleet's listeners bounded by
/// [`HANG`] ([`peer_read_within`]), for a read whose every listener serves in process, refuses at
/// once or is held silent ([`member::HeldSilent`]): each answers or ends at once, so the read
/// waits on nothing but the hang detector.
fn with_peers_answering<R>(act: impl FnOnce() -> R) -> R {
    let before = PEER_READ_WITHIN.replace(Some(HANG));
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(act));
    PEER_READ_WITHIN.set(before);
    done.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn with_aterm_here<R>(hook: impl FnMut() -> Here + 'static, act: impl FnOnce() -> R) -> R {
    ATERM_HERE.with(|here| *here.borrow_mut() = Some(Box::new(hook)));
    let done = std::panic::catch_unwind(std::panic::AssertUnwindSafe(act));
    ATERM_HERE.with(|here| *here.borrow_mut() = None);
    done.unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

#[test]
fn a_first_init_waits_for_the_listener_of_an_aterm_running_here() {
    let issuer = World::new("init-binds");
    let port = closed_port();
    let state = issuer.places.state.clone();
    let bound: Rc<RefCell<Option<Listener>>> = Rc::default();
    let fresh = with_aterm_here(
        {
            let bound = Rc::clone(&bound);
            // The aterm here reads [fleet] and binds its listener while the init waits.
            move || {
                if bound.borrow().is_none() {
                    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
                    *bound.borrow_mut() = Some(Listener::serve(listener, &state));
                }
                Here::Answers(Some("off".to_owned()))
            }
        },
        || {
            ok(init(
                &issuer.places,
                &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
            ))
        },
    );
    assert!(bound.borrow().is_some(), "the init asked for an aterm here");
    assert!(!fresh.contains("  listener "), "{fresh}");
    assert!(fresh.contains(FOLLOWS), "{fresh}");
    let code = vault_code(&issuer);
    let member = World::new("init-binds-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));

    let unbound = World::new("init-unbound");
    let port = closed_port();
    let waited = with_aterm_here(
        || Here::Answers(Some("off".to_owned())),
        || {
            ok(init(
                &unbound.places,
                &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
            ))
        },
    );
    assert!(
        waited.contains(&format!(
            "  listener   nothing listens on 127.0.0.1:{port} 3 s after the new key was written: \
             the aterm running here reads fleet=off, so it has not read [fleet] yet, or it reads \
             another aterm.toml than {}, whose [fleet] is off",
            unbound.places.config.display()
        )),
        "{waited}"
    );
    assert!(
        !waited.contains(FOLLOWS)
            && waited.contains(&format!("`--at 127.0.0.1:{port}` names this one.\n")),
        "no close that says it listens: {waited}"
    );
}

#[test]
fn an_init_beside_an_aterm_that_answers_nothing_says_so_at_once() {
    // A listener bound before its aterm stopped takes a connection, and answers nothing: held
    // silent, a dial that connects there ends at its handshake at once.
    let bound = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let held_silent = member::HeldSilent::at(bound.local_addr().unwrap());
    let held = bound.local_addr().unwrap().port();
    let closed = closed_port();
    let unbound = format!("nothing listens on 127.0.0.1:{closed}");
    let stalled = format!("127.0.0.1:{held} takes a connection but finishes no fleet handshake");
    for (port, here, line) in [
        (
            closed,
            Here::Stopped(4242),
            format!("{unbound}: the aterm running here (pid 4242) is stopped"),
        ),
        (
            closed,
            Here::Silent(Some(4242)),
            format!(
                "{unbound}: the aterm running here (pid 4242) takes a connection but answered no \
                 request"
            ),
        ),
        (
            held,
            Here::Stopped(4242),
            format!(
                "{stalled}: the aterm running here (pid 4242) is stopped, so a listener it bound \
                 takes a connection in the kernel and answers nothing: `kill -CONT 4242` resumes it"
            ),
        ),
        (
            held,
            Here::Silent(Some(4242)),
            format!(
                "{stalled}: the aterm running here (pid 4242) answered no request either, so it is \
                 frozen or too busy to answer"
            ),
        ),
    ] {
        let world = World::new("init-silent");
        let asked = Rc::new(Cell::new(0));
        let said = with_aterm_here(
            {
                let (asked, here) = (Rc::clone(&asked), here.clone());
                move || {
                    asked.set(asked.get() + 1);
                    here.clone()
                }
            },
            || {
                ok(init(
                    &world.places,
                    &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
                ))
            },
        );
        assert_eq!(asked.get(), 1, "{here:?} is not waited out: {said}");
        assert!(
            said.contains(&format!("  listener   {line}")) && !said.contains(FOLLOWS),
            "{said}"
        );
        if port == held {
            let listed = with_aterm_here(
                move || here.clone(),
                || with_peers_answering(|| ok(peers(&world.places, &[]))),
            );
            let head = listed.lines().next().unwrap_or_default();
            assert!(
                head.contains(&format!(
                    "([fleet] listen is 127.0.0.1:{held} and allow is facts, but {}",
                    line.replacen(
                        "finishes no fleet handshake",
                        "finishes no fleet handshake just now",
                        1
                    )
                )) && !head.contains("another program holds that port"),
                "{listed}"
            );
        }
    }
    drop(held_silent);
    drop(bound);
}

#[test]
fn a_join_or_init_whose_listen_place_another_program_holds_never_says_it_listens() {
    // Another program holds the place: it takes a connection and sends no fleet handshake. Held
    // silent, a dial that connects there ends at its handshake at once, as the squatter's deadline
    // ends it ([`member::HeldSilent`]), so each verb waits on nothing but its own window.
    let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let held_silent = member::HeldSilent::at(squatter.local_addr().unwrap());
    let held = squatter.local_addr().unwrap().port();
    let stalled = format!("127.0.0.1:{held} takes a connection but finishes no fleet handshake");
    let waited = " 3 s after the new key was written";
    let retrying = || Here::Answers(Some("retrying".to_owned()));
    let binds = "another program holds that port, so the aterm running here reads fleet=retrying, \
                 and binds it once that program lets it go";
    let rotate = "`aterm fleet init --rotate --port <n>` moves it (every member joins again)";
    let cases = [
        (retrying(), format!("{stalled}{waited}: {binds}; {rotate}")),
        (
            Here::Absent,
            format!(
                "{stalled}: another program holds that port, so no aterm runs here, and one \
                 started here reads fleet=retrying until that program lets it go; {rotate}"
            ),
        ),
        // Its own word, never retrying: macOS binds a wildcard listen beside a loopback one.
        (
            Here::Answers(Some("listening".to_owned())),
            format!(
                "{stalled}{waited}, though the aterm running here reads fleet=listening: another \
                 program holds that port beside its wildcard listen (this machine's own dials of \
                 the loopback reach that program, and a peer's of this Mac's LAN address the \
                 aterm) or while it listens elsewhere, or the aterm is too busy to finish one in \
                 time; {rotate}"
            ),
        ),
        (
            Here::Answers(Some("off".to_owned())),
            format!(
                "{stalled}{waited}: another program holds that port, so the aterm running here \
                 reads fleet=off (`aterm ctl status` here), and once it tries this [fleet] \
                 listen, fleet=retrying until that program lets it go; {rotate}"
            ),
        ),
    ];
    // Each verb waits out its own window on a thread of its own, its aterm here with it.
    std::thread::scope(|scope| {
        let joined = scope.spawn(|| {
            let (_issuer, code, _serving) = issuing("join-squatted-issuer");
            let member = World::new("join-squatted-member");
            let opts = Opts {
                port: Some(held),
                host: Some("127.0.0.1".to_owned()),
                ..Opts::default()
            };
            let joined = with_aterm_here(retrying, || ok(join(&member.places, &opts, &code)));
            assert!(
                joined.contains(&format!(
                    "  listener   {stalled}{waited}: {binds}; `aterm fleet join --port <n>` moves \
                     it\n"
                )) && !joined.contains(FOLLOWS),
                "{joined}"
            );
        });
        aterm_spec::check::each(&cases, |(here, line)| {
            let world = World::new("init-squatted");
            let here = here.clone();
            let said = with_aterm_here(
                move || here.clone(),
                || {
                    ok(init(
                        &world.places,
                        &args(&["--host", "127.0.0.1", "--port", &held.to_string()]),
                    ))
                },
            );
            assert!(
                said.contains(&format!("  listener   {line}\n")) && !said.contains(FOLLOWS),
                "{said}"
            );
        });
        joined
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
    });
    drop(held_silent);
    drop(squatter);
}

#[test]
fn an_init_after_a_leave_waits_out_a_listener_that_cuts_its_probe() {
    let (issuer, _, old) = issuing("init-cut");
    ok(leave(&issuer.places, &[]));
    drop(old);
    // The aterm here's listener between off and on: it cuts two connections, then serves.
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let state = issuer.places.state.clone();
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let cutter = std::thread::spawn(move || {
        for _ in 0..2 {
            drop(held.accept());
        }
        let serving = Listener::rereading(held, &state);
        let _ = stopped.recv();
        drop(serving);
    });
    let fresh = with_aterm_here(
        || Here::Answers(Some("listening".to_owned())),
        || {
            ok(init(
                &issuer.places,
                &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
            ))
        },
    );
    assert!(
        !fresh.contains("  listener ") && fresh.contains(FOLLOWS),
        "{fresh}"
    );
    let code = vault_code(&issuer);
    let member = World::new("init-cut-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    drop(stop);
    cutter.join().unwrap();
}

#[test]
fn a_peers_header_never_calls_a_listener_the_aterm_here_refused_serving() {
    let (_issuer, code, _serving) = issuing("refused-header-issuer");
    let member = World::new("refused-header-member");
    let port = closed_port();
    let opts = Opts {
        port: Some(port),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let head = |listed: &str| listed.lines().next().unwrap_or_default().to_owned();
    let list = || with_peers_answering(|| ok(peers(&member.places, &[])));
    let refused = with_aterm_here(|| Here::Answers(Some("refused:allow".to_owned())), list);
    assert!(
        head(&refused).contains(&format!(
            "([fleet] listen is 127.0.0.1:{port} and allow is facts, but nothing listens on \
             127.0.0.1:{port} just now: the aterm running here refused the listen \
             (fleet=refused:allow): the [fleet] allow it read is none of \"facts\", \"content\" \
             and \"drive\", though {} names one, so it has not read it yet, or it reads another \
             aterm.toml",
            member.places.config.display()
        )) && !head(&refused).contains("serving"),
        "{refused}"
    );
    let config = member.config();
    member.write_config(&config.replace("allow = \"facts\"", "allow = \"bogus\""));
    let bogus = list();
    assert!(
        head(&bogus).contains(
            "([fleet] allow is \"bogus\", none of \"facts\", \"content\" and \"drive\", so no \
             aterm here listens for the fleet (`aterm ctl status` here reads \
             fleet=refused:allow): `aterm fleet allow facts` (or `content`) writes one in "
        ) && !head(&bogus).contains("serving"),
        "{bogus}"
    );
    // The aterm here listens by another aterm.toml, or has not read this one yet.
    let serving = Listener::serve(
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap(),
        &member.places.state,
    );
    let proved = head(&list());
    assert!(
        proved.contains(&format!(
            "(it listens on 127.0.0.1:{port}, though [fleet] allow is \"bogus\", none of \
             \"facts\", \"content\" and \"drive\": the aterm listening there has not read it yet"
        )) && !proved.contains("no aterm here listens")
            && !proved.contains("serving"),
        "{proved}"
    );
    drop(serving);
    ok(allow(&member.places, &args(&["facts"])));
    assert_eq!(member.config(), config, "the remedy it names mends it");
}

#[test]
fn a_join_after_a_leave_waits_for_this_machines_listener_to_serve_the_new_key() {
    let (_one, first, _first) = issuing("join-serves-first");
    let (_two, second, _second) = issuing("join-serves-second");
    let member = World::new("join-serves-member");
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    let opts = Opts {
        port: Some(port),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    {
        // Bound and not served yet: the first join's check of its own place and its proof of the
        // listener there end at the handshake at once ([`member::HeldSilent`]).
        let _silent = member::HeldSilent::at(held.local_addr().unwrap());
        ok(join(&member.places, &opts, &first));
    }
    let stuck = Listener::serve(held, &member.places.state);
    ok(leave(&member.places, &[]));
    let joined = ok(join(&member.places, &opts, &second));
    assert!(
        joined.contains(&format!(
            "  listener   127.0.0.1:{port} still answers under another secret 3 s after the new \
             key was written"
        )),
        "{joined}"
    );
    assert!(
        !joined.contains(FOLLOWS) && joined.contains("\nOn a Mac, the local-network consent"),
        "no close that says it listens: {joined}"
    );
    drop(stuck);
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
    world.write_config(&format!(
        "[[fleet.peers]]\nhost = \"lab-mini:{}\"\n[[fleet.peers]]\nhost = \"10.0.0.5\"\n\
         [[fleet.peers]]\nhost = \"lab-mini:7101\"\n[[fleet.peers]]\nhost = \"lab-maxi\"\n",
        member::default_port()
    ));
    member::ensure_private_dir(&world.places.state).unwrap();
    let joined = Peer {
        node: Some("ab".repeat(16)),
        key: None,
        key_from: None,
        host: "lab-mini.local".to_owned(),
        addr: Some(IpAddr::from([10, 0, 0, 5])),
        port: member::default_port(),
        version: None,
        seen_ms: None,
        via: Via::Joined,
        fleet: None,
        signed: None,
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
        [
            format!("10.0.0.5:{}", member::default_port()),
            "lab-mini:7101".to_owned(),
            format!("lab-maxi:{}", member::default_port())
        ],
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
    let code = vault_code(&issuer);
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
    let code = vault_code(&issuer);
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
    let code = vault_code(&issuer);
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
            .map(|row| (row.peer.via, row.configured.is_some()))
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
fn an_issuer_whose_book_lists_no_peer_says_so_and_not_that_none_joined() {
    let (issuer, code, _serving) = issuing("alone-issuer");
    let fleet = aterm_ctl::FleetAsk {
        host: None,
        within: HANG,
    };
    let book = "this machine's book lists no peer: a member is recorded when it runs `aterm fleet \
                join`, and `aterm fleet peers add <host>` names one by hand";
    let alone = || {
        let read = survey_fleet_in(&issuer.places, &fleet).unwrap();
        assert!(read.peers.is_empty(), "{:?}", read.peers);
        let (why, listed) = (
            read.alone.unwrap_or_default(),
            ok(list_peers(&issuer.places, HANG)),
        );
        assert!(
            why == format!("survey --fleet read no other machine: {book}")
                && listed.ends_with(&format!("  none: {book}\n")),
            "{why}\n{listed}"
        );
    };
    alone();
    let member = World::new("alone-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let node = member::node_id(&member.places.state).unwrap();
    ok(peers(&issuer.places, &args(&["rm", short(&node)])));
    alone();
}

#[test]
fn a_members_rm_of_its_issuer_gives_a_members_remedy() {
    let (issuer, code, _serving) = issuing("rm-issuer-issuer");
    let member = World::new("rm-issuer-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let removed = ok(with_issuer_serving(|| {
        peers(&member.places, &args(&["rm", short(&issuer_node)]))
    }));
    assert!(
        !removed.contains("keep the fleet's secret")
            && removed.ends_with(
                "  issuer     this machine names no issuer now, so it reads only the peers named \
                 here: `aterm fleet join` with the issuer's join code binds it again\n"
            ),
        "{removed}"
    );
    let listed = ok(list_peers(&member.places, HANG));
    assert!(
        listed.contains("names no issuer, so it reads no member: `aterm fleet join` with the issuer's join code binds it again")
            && !listed.contains("a member is recorded"),
        "{listed}"
    );
    let fleet = aterm_ctl::FleetAsk {
        host: None,
        within: HANG,
    };
    let alone = survey_fleet_in(&member.places, &fleet).unwrap();
    assert!(
        alone.peers.is_empty()
            && alone.alone.as_deref().is_some_and(|why| why.starts_with(
                "survey --fleet read no other machine: this machine holds the fleet's key and \
                 names no issuer, so it reads no member: `aterm fleet join` with the issuer's \
                 join code binds it again"
            )),
        "{:?}",
        alone.alone
    );
    let named = format!("127.0.0.1:{}", closed_port());
    ok(peers(&member.places, &args(&["add", &named])));
    let read = survey_fleet_in(&member.places, &fleet).unwrap();
    assert_eq!(
        (read.peers.len(), read.alone),
        (1, None),
        "a peer named by hand is read"
    );
    let listed = ok(list_peers(&member.places, HANG));
    assert!(
        listed.ends_with(
            "This machine holds the fleet's key and names no issuer, so it reads only the peers \
             named here: `aterm fleet join` with the issuer's join code binds it again.\n"
        ),
        "{listed}"
    );
    let joined = ok(join(&member.places, &opts, &code));
    assert!(joined.contains("recorded this machine"), "{joined}");
    assert!(
        !ok(list_peers(&member.places, HANG)).contains("names no issuer"),
        "the join binds it again"
    );
}

#[test]
fn a_word_naming_the_issuer_and_a_copied_member_is_refused_by_every_verb_alike() {
    let (issuer, code, _serving) = issuing("alike-issuer");
    let member = World::new("alike-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let node = "e5".repeat(16);
    let other = closed_port();
    member::update_peers(&issuer.places.state, |book| {
        book.push(Peer {
            node: Some(node.clone()),
            key: Some(NodeIdentity::from_seed(&node, &[8; 32]).unwrap().key()),
            key_from: None,
            host: "127.0.0.1".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: other,
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
            signed: None,
        });
        Ok(())
    })
    .unwrap();
    let _ = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let book = read_peers(&member.places.state).unwrap();
    let copy_file = member.places.state.join(ISSUER_BOOK_FILE);
    assert!(copy_file.exists());
    let named = format!(
        "`127.0.0.1` names 2 peers ({issuer_node} at {}, {node} at 127.0.0.1:{other}): name one by \
         its node id or its address and port",
        book[0].target(),
    );

    with_issuer_serving(|| {
        let removed = peers(&member.places, &args(&["rm", "127.0.0.1"]));
        assert!(
            matches!(&removed, Err(Failure::Failed(why)) if *why == named),
            "{removed:?}"
        );
        assert_eq!(read_peers(&member.places.state).unwrap(), book);
        assert!(copy_file.exists(), "the copy stays");
        let granted = allow(&member.places, &args(&["drive", "--from", "127.0.0.1"]));
        assert!(
            matches!(&granted, Err(Failure::Failed(why)) if *why == named),
            "{granted:?}"
        );
        let ask = aterm_ctl::FleetAsk {
            host: Some("127.0.0.1"),
            within: 2 * HANG,
        };
        let surveyed = survey_fleet_in(&member.places, &ask).map(|read| read.peers.len());
        assert_eq!(surveyed.map_err(|refused| refused.why), Err(named.clone()));
        let driven = drive_fleet_in(&member.places, "127.0.0.1", HANG)
            .err()
            .expect("refused before any dial");
        assert_eq!(driven.why, named);
    });
}

#[test]
fn a_word_naming_the_issuer_and_an_entry_by_hand_elsewhere_takes_out_neither() {
    let (issuer, code, _serving) = issuing("by-hand-issuer");
    let member = World::new("by-hand-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let (copied, configured) = (closed_port(), closed_port());
    ok(peers(
        &issuer.places,
        &args(&["add", &format!("127.0.0.1:{copied}")]),
    ));
    let _ = check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    let copy_file = member.places.state.join(ISSUER_BOOK_FILE);
    assert!(
        std::fs::read_to_string(&copy_file)
            .unwrap()
            .contains(&format!("port={copied} ")),
        "the copy holds the issuer's hand entry"
    );
    let book = read_peers(&member.places.state).unwrap();
    let issuer_at = format!(
        "{} at {}",
        member::node_id(&issuer.places.state).unwrap(),
        book[0].target()
    );
    let refused = |named: &str| {
        with_issuer_serving(|| {
            let removed = peers(&member.places, &args(&["rm", "127.0.0.1"]));
            assert!(
                matches!(&removed, Err(Failure::Failed(why)) if why == named),
                "{removed:?}"
            );
            assert_eq!(read_peers(&member.places.state).unwrap(), book);
            assert!(copy_file.exists(), "the copy stays");
            let ask = aterm_ctl::FleetAsk {
                host: Some("127.0.0.1"),
                within: 2 * HANG,
            };
            let surveyed = survey_fleet_in(&member.places, &ask).map(|read| read.peers.len());
            assert_eq!(
                surveyed.map_err(|refused| refused.why),
                Err(named.to_owned())
            );
            let driven = drive_fleet_in(&member.places, "127.0.0.1", HANG)
                .err()
                .expect("refused before any dial");
            assert_eq!(driven.why, named);
        });
    };
    refused(&format!(
        "`127.0.0.1` names 2 peers ({issuer_at}, - at 127.0.0.1:{copied}): name one by its node \
         id or its address and port"
    ));
    member.write_config(&format!(
        "{}\n[[fleet.peers]]\nhost = \"127.0.0.1:{configured}\"\n",
        member.config()
    ));
    refused(&format!(
        "`127.0.0.1` names 3 peers ({issuer_at}, - at 127.0.0.1:{copied}, - at \
         127.0.0.1:{configured}): name one by its node id or its address and port"
    ));
}

#[test]
fn a_word_naming_the_issuer_gets_one_answer_from_every_verb_from_an_old_copy_or_none() {
    let (issuer, code, serving) = issuing("no-copy-issuer");
    let member = World::new("no-copy-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let copy_file = member.places.state.join(ISSUER_BOOK_FILE);
    assert!(!copy_file.exists(), "a join keeps no copy");
    let (copied, kept) = ("e5".repeat(16), closed_port());
    joins(&issuer, &copied, kept);
    ok(list_peers(&member.places, HANG));
    let old_copy = std::fs::read(&copy_file).unwrap();
    let node = "e6".repeat(16);
    let other = closed_port();
    joins(&issuer, &node, other);
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let book = read_peers(&member.places.state).unwrap();
    let refusals = |before: &dyn Fn()| {
        let ask = aterm_ctl::FleetAsk {
            host: Some("127.0.0.1"),
            within: 2 * HANG,
        };
        before();
        let removed = peers(&member.places, &args(&["rm", "127.0.0.1"]));
        before();
        let granted = allow(&member.places, &args(&["drive", "--from", "127.0.0.1"]));
        before();
        let surveyed = survey_fleet_in(&member.places, &ask).map(|read| read.peers.len());
        before();
        let driven = drive_fleet_in(&member.places, "127.0.0.1", HANG).map(|link| link.label);
        assert_eq!(read_peers(&member.places.state).unwrap(), book);
        let again = driven.as_ref().is_err_and(|refused| refused.again);
        let words = |failure: Result<String, Failure>| failure.map_err(failure_words);
        let said = vec![
            words(removed),
            words(granted),
            surveyed
                .map(|n| n.to_string())
                .map_err(|refused| refused.why),
            driven.map_err(|refused| refused.why),
        ];
        (said, again)
    };
    let gone = || {
        let _ = std::fs::remove_file(&copy_file);
    };
    let old = || write_private(&member.places.state, ISSUER_BOOK_FILE, &old_copy).unwrap();
    let named = format!(
        "`127.0.0.1` names 3 peers ({issuer_node} at {}, {copied} at 127.0.0.1:{kept}, {node} at \
         127.0.0.1:{other}): name one by its node id or its address and port",
        book[0].target(),
    );
    with_issuer_serving(|| {
        for before in [&gone as &dyn Fn(), &old] {
            assert_eq!(refusals(before), (vec![Err(named.clone()); 4], false));
        }
    });

    drop(serving);
    let lead = format!(
        "no book here says which members `127.0.0.1` names beside this machine's issuer ({} at \
         {}), since its issuer's book could not be read (",
        short(&issuer_node),
        book[0].target()
    );
    let remedy = format!(
        ": name it by its node id ({issuer_node}) or its address and port ({}), or try again \
         once it answers",
        book[0].target()
    );
    for before in [&gone as &dyn Fn(), &old] {
        // Nothing listens at the issuer's port, which refuses each read at once.
        let (unknown, again) = with_issuer_read_within(HANG, || refusals(before));
        assert!(
            unknown[0]
                .as_ref()
                .is_err_and(|why| why.starts_with(&lead) && why.ends_with(&remedy)),
            "{unknown:?}"
        );
        assert_eq!(unknown, vec![unknown[0].clone(); 4]);
        assert!(
            again,
            "a drive refused for a silent issuer is one to send again"
        );
    }
    let removed = ok(peers(&member.places, &args(&["rm", &book[0].target()])));
    assert!(
        removed.starts_with(&format!(
            "aterm fleet peers: removed {} ({}), and this machine's copy of its book",
            book[0].target(),
            short(&issuer_node)
        )),
        "{removed}"
    );
}

fn joins(issuer: &World, node: &str, port: u16) {
    member::update_peers(&issuer.places.state, |book| {
        book.push(Peer {
            node: Some(node.to_owned()),
            key: Some(NodeIdentity::from_seed(node, &[9; 32]).unwrap().key()),
            key_from: None,
            host: "127.0.0.1".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port,
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
            signed: None,
        });
        Ok(())
    })
    .unwrap();
}

#[test]
fn a_members_peers_rm_never_says_no_peer_for_a_word_its_issuers_unread_book_may_name() {
    let (issuer, code, serving) = issuing("rm-unread-issuer");
    let member = World::new("rm-unread-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let granted = "e6".repeat(16);
    joins(&issuer, &granted, closed_port());
    ok(with_issuer_serving(|| {
        allow(&member.places, &args(&["drive", "--from", &granted]))
    }));
    // A member that joined since the copy: the rm reads the issuer's book again, as a drive does.
    let since = "e7".repeat(16);
    joins(&issuer, &since, closed_port());
    let copied = with_issuer_serving(|| peers(&member.places, &args(&["rm", &since])))
        .map_err(failure_words);
    assert_eq!(
        copied,
        Err(format!(
            "`{since}` is in the fleet issuer's book, which this machine copies at each read: \
             `aterm fleet peers rm` on the issuer takes it out, and this copy follows"
        ))
    );
    std::fs::remove_file(member.places.state.join(ISSUER_BOOK_FILE)).unwrap();
    drop(serving);
    let config = member.config();
    // Nothing listens at the issuer's port, which refuses the read at once.
    let unknown = with_issuer_read_within(HANG, || peers(&member.places, &args(&["rm", &granted])))
        .map_err(failure_words);
    let lead = format!(
        "`{granted}` names no peer this machine knows, and its issuer's book could not be read ("
    );
    let tail = format!(
        ", so no book here says which Mac it is, and nothing was removed (node {}'s drive grant \
         stays): try again once it answers (`aterm fleet peers` says why)",
        short(&granted)
    );
    assert!(
        unknown
            .as_ref()
            .is_err_and(|why| why.starts_with(&lead) && why.ends_with(&tail)),
        "{unknown:?}"
    );
    assert_eq!(member.config(), config, "the grant stays, and says so");
}

#[test]
fn a_members_peers_rm_a_leave_comes_into_refuses_as_one_after_the_leave_would() {
    let (_issuer, code, serving) = issuing("rm-left-issuer");
    let member = World::new("rm-left-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    drop(serving);
    let word = "e6".repeat(16);
    let places = Places {
        state: member.places.state.clone(),
        config: member.places.config.clone(),
    };
    let left = Rc::new(RefCell::new(None));
    let hook = {
        let left = left.clone();
        move |step: &str| {
            if step == "rm" && left.borrow().is_none() {
                *left.borrow_mut() = Some(leave(&places, &[]).map_err(failure_words));
            }
        }
    };
    // The leave lands while the rm read its silent issuer's book, before the book's lock.
    let raced = with_step_hook(hook, || peers(&member.places, &args(&["rm", &word])))
        .map_err(failure_words);
    assert!(matches!(left.borrow_mut().take(), Some(Ok(_))));
    let serial = peers(&member.places, &args(&["rm", &word])).map_err(failure_words);
    assert!(
        matches!(&serial, Err(why) if !why.contains("did not answer") && !why.contains("could not be read")),
        "{serial:?}"
    );
    assert_eq!(raced, serial);
}

#[test]
fn the_remedy_for_another_node_at_the_issuers_place_takes_the_issuer_out() {
    let issuer = World::new("displaced-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let code = vault_code(&issuer);
    let serving = Listener::serve(listener, &issuer.places.state);
    let opts = || Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    let member = World::new("displaced-member");
    ok(join(&member.places, &opts(), &code));
    let stand_in = World::new("displaced-stand-in");
    ok(join(&stand_in.places, &opts(), &code));
    drop(serving);
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let _standing = Listener::serve(listener, &stand_in.places.state);
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let stand_in_node = member::node_id(&stand_in.places.state).unwrap();

    let listed = ok(list_peers(&member.places, HANG));
    let row = listed
        .lines()
        .find(|line| line.trim_start().starts_with(short(&issuer_node)))
        .unwrap_or_else(|| panic!("{listed}"));
    let lead = format!(
        "node {} is here: `aterm fleet peers rm ",
        short(&stand_in_node)
    );
    let word = row
        .split_once(&lead)
        .and_then(|(_, rest)| rest.split_once('`'))
        .map(|(word, _)| word)
        .unwrap_or_else(|| panic!("{row}"));
    assert_eq!(word, issuer_node, "{row}");
    let granted = allow(
        &member.places,
        &args(&["drive", "--from", short(&issuer_node)]),
    )
    .map_err(failure_words);
    assert!(
        granted.as_ref().is_err_and(|why| {
            why.starts_with("no book here says which members")
                && !why.contains(&format!("peers rm {}`", short(&issuer_node)))
        }),
        "{granted:?}"
    );
    let removed = ok(peers(&member.places, &args(&["rm", word])));
    assert!(
        removed.starts_with(&format!(
            "aterm fleet peers: removed 127.0.0.1:{port} ({})",
            short(&issuer_node)
        )),
        "{removed}"
    );
}

#[test]
fn a_leave_names_each_tab_this_machine_holds_on_another_mac() {
    let issuer = World::new("leave-held-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let code = vault_code(&issuer);
    let serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("leave-held-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    drop(serving);
    let node = member::node_id(&member.places.state).unwrap();
    let sid = "s-0123456789abcdef0123";
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let _holding = Listener::with_rows(
        listener,
        &issuer.places.state,
        vec![
            format!("session sid={sid} held_from={node}"),
            "session sid=s-00000000000000000002 held_from=-".to_owned(),
            format!("session sid=\u{1b}]52;c;aGk=\u{7} held_from={node}"),
            format!("session sid=s-0123zz held_from={node}"),
        ],
    );

    let left = ok(with_peers_answering(|| leave(&member.places, &[])));
    assert!(
        !left.contains('\u{1b}') && !left.contains("s-0123zz"),
        "{left:?}"
    );
    let hold = format!(
        "aterm fleet leave:\n  hold       {sid} on 127.0.0.1:{port} stays held by this machine, \
         which reaches that Mac no more once it has left: that Mac lifts the hold when its \
         person takes the tab (Take Over), runs `aterm fleet peers rm {node}` there, or lowers \
         `aterm fleet allow` there (to content or facts), and so does `aterm ctl --host \
         127.0.0.1:{port} hold {sid} off` here once this machine joins again\n  off        "
    );
    assert!(left.starts_with(&hold), "{left}");
    assert_eq!(left.matches("  hold  ").count(), 1, "{left}");
    match drive_fleet_in(&member.places, &format!("127.0.0.1:{port}"), HANG) {
        Err(refused) => assert_eq!(
            refused.left,
            Some(aterm_ctl::Left::Fleet(node)),
            "{refused}"
        ),
        Ok(_) => panic!("a drive with no fleet's key"),
    }
}

#[test]
fn a_leave_names_the_issuers_peers_rm_for_a_tab_it_holds_on_another_member() {
    let (_issuer, code, _serving) = issuing("leave-member-issuer");
    let (holder, held) = (
        World::new("leave-member-holder"),
        World::new("leave-member-held"),
    );
    let port = closed_port();
    ok(join(
        &held.places,
        &Opts {
            port: Some(port),
            ..Opts::default()
        },
        &code,
    ));
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&holder.places, &opts, &code));
    let node = member::node_id(&holder.places.state).unwrap();
    let sid = "s-0123456789abcdef0abc";
    let _holding = Listener::with_rows(
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap(),
        &held.places.state,
        vec![format!("session sid={sid} held_from={node}")],
    );

    let left = ok(leave(&holder.places, &[]));
    let hold = format!(
        "  hold       {sid} on 127.0.0.1:{port} stays held by this machine, which reaches that \
         Mac no more once it has left: that Mac lifts the hold when its person takes the tab \
         (Take Over) or lowers `aterm fleet allow` there (to content or facts), or once the \
         fleet's issuer takes this machine out (`aterm fleet peers rm {node}` on the issuer) and \
         that Mac reads the issuer's book again (`aterm fleet peers` there reads it now), and so \
         does `aterm ctl --host 127.0.0.1:{port} hold {sid} off` here once this machine joins \
         again\n"
    );
    assert!(left.contains(&hold), "{left}");
    assert!(!left.contains(&format!("peers rm {node}` there")), "{left}");
}

#[test]
fn a_leave_names_each_mac_its_read_could_not_check_for_the_tabs_it_holds() {
    let (issuer, code, serving) = issuing("leave-unread-issuer");
    let member = World::new("leave-unread-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let port = hinted_port(&code);
    let node = member::node_id(&member.places.state).unwrap();
    drop(serving);
    drop(issuer);

    let left = ok(leave(&member.places, &[]));
    let unread = format!(
        "aterm fleet leave:\n  unread     127.0.0.1:{port}: this machine could not read it just \
         now ("
    );
    let stays = format!(
        "so it cannot say which tabs it holds there, and any it holds stays held once it has \
         left: that Mac lifts the hold when its person takes the tab (Take Over), runs `aterm \
         fleet peers rm {node}` there"
    );
    assert!(
        left.starts_with(&unread)
            && left.contains(&stays)
            && left.contains("  unread     its issuer's book was not read just now (")
            && left.find("  unread  ") < left.find("  removed  "),
        "{left}"
    );
}

#[test]
fn a_join_of_another_fleet_names_each_tab_this_machine_holds_in_the_old_one() {
    let issuer = World::new("join-held-issuer");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let code = vault_code(&issuer);
    let serving = Listener::serve(listener, &issuer.places.state);
    let member = World::new("join-held-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    drop(serving);
    let node = member::node_id(&member.places.state).unwrap();
    let sid = "s-0123456789abcdef0789";
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let _holding = Listener::with_rows(
        listener,
        &issuer.places.state,
        vec![format!("session sid={sid} held_from={node}")],
    );
    let (_other, other_code, _other_serving) = issuing("join-held-other");

    let joined = ok(with_peers_answering(|| {
        join(&member.places, &opts, &other_code)
    }));
    let hold = format!(
        "  hold       {sid} on 127.0.0.1:{port} stays held by this machine, which reaches that \
         Mac no more now that it holds another fleet's key: that Mac lifts the hold when its \
         person takes the tab (Take Over), runs `aterm fleet peers rm {node}` there, or lowers \
         `aterm fleet allow` there (to content or facts), and so does `aterm ctl --host \
         127.0.0.1:{port} hold {sid} off` here once this machine joins that fleet again\n"
    );
    assert!(joined.contains(&hold), "{joined}");
    assert_eq!(joined.matches("  hold  ").count(), 1, "{joined}");
    match with_issuer_serving(|| drive_fleet_in(&member.places, &format!("127.0.0.1:{port}"), HANG))
    {
        Err(refused) => assert_eq!(refused.left, Some(aterm_ctl::Left::Book(node)), "{refused}"),
        Ok(_) => panic!("a drive of a Mac the book records no more"),
    }
    let again = ok(join(&member.places, &opts, &other_code));
    assert!(
        !again.contains("  hold  "),
        "a join again of the fleet held: {again}"
    );
}

#[test]
fn a_join_of_a_rotated_code_reads_no_other_member_for_its_holds() {
    let (issuer, code, serving) = issuing("rotate-unread-issuer");
    let member = World::new("rotate-unread-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let old = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    old.set_nonblocking(true).unwrap();
    member.write_config(&format!(
        "{}\n[[fleet.peers]]\nhost = \"127.0.0.1:{}\"\n",
        member.config(),
        old.local_addr().unwrap().port()
    ));
    let port = hinted_port(&code);
    drop(serving);
    ok(init(&issuer.places, &args(&["--rotate"])));
    let rotated = vault_code(&issuer);
    let _serving = Listener::serve(
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap(),
        &issuer.places.state,
    );

    let joined = ok(join(&member.places, &opts, &rotated));
    assert!(
        joined.contains("in place of the secret its issuer held before\n"),
        "{joined}"
    );
    assert!(
        matches!(old.accept(), Err(e) if e.kind() == io::ErrorKind::WouldBlock),
        "a rotate's join dialled another member: {joined}"
    );
}

#[test]
fn an_issuers_leave_names_no_join_again_for_a_tab_it_holds() {
    let (issuer, code, serving) = issuing("leave-held-by-issuer");
    let member = World::new("leave-held-granter");
    let port = closed_port();
    let opts = Opts {
        port: Some(port),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let node = member::node_id(&issuer.places.state).unwrap();
    let sid = "s-0123456789abcdef0456";
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let _holding = Listener::with_rows(
        listener,
        &member.places.state,
        vec![format!("session sid={sid} held_from={node}")],
    );
    ok(list_peers(&member.places, HANG));
    assert!(member.places.state.join(ISSUER_BOOK_FILE).exists());

    let left = ok(with_peers_answering(|| leave(&issuer.places, &[])));
    let hold = format!(
        "aterm fleet leave:\n  hold       {sid} on 127.0.0.1:{port} stays held by this machine, \
         which reaches that Mac no more once it has left: that Mac lifts the hold when its \
         person takes the tab (Take Over), runs `aterm fleet peers rm {node}` there, or lowers \
         `aterm fleet allow` there (to content or facts)\n  off        "
    );
    assert!(left.starts_with(&hold), "{left}");
    assert!(
        !left.contains("joins again") && left.contains("its fleet went with it"),
        "{left}"
    );
    // The leave took [fleet] listen out, so the aterm there serves the fleet no more.
    drop(serving);
    let refused = peers(&member.places, &args(&["rm", &node[..8]])).map_err(failure_words);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.starts_with("no book here says which members")),
        "{refused:?}"
    );
    let removed = ok(peers(&member.places, &args(&["rm", &node])));
    assert!(
        removed.starts_with("aterm fleet peers: removed ")
            && removed.contains(&format!(
                "({}), and this machine's copy of its book",
                &node[..8]
            )),
        "{removed}"
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
    ok(peers(&world.places, &args(&["add", "lab-maxi"])));
    assert_eq!(
        hosts(),
        [
            "lab-mini:7101".to_owned(),
            format!("lab-maxi:{}", member::default_port())
        ],
        "a host alone is at the default port"
    );
    let shown: Vec<String> = read_peers(&world.places.state)
        .unwrap()
        .iter()
        .map(shown_name)
        .collect();
    assert_eq!(
        shown,
        ["lab-mini:7101", "lab-maxi"],
        "named with no default port"
    );
    ok(peers(&world.places, &args(&["rm", "lab-maxi"])));
    assert_eq!(hosts(), ["lab-mini:7101"]);
    let config = world.config();
    world.write_config(&format!("{config}\n[[fleet.peers]]\nhost = \"lab-maxi\"\n"));
    let at_default = format!("lab-maxi:{}", member::default_port());
    match peers(&world.places, &args(&["rm", &at_default])) {
        Err(Failure::Failed(why)) => assert!(
            why.contains("aterm.toml's `[[fleet.peers]] host = \"lab-maxi\"` names it"),
            "an entry with no port is at the default port: {why}"
        ),
        other => panic!("{other:?}"),
    }
    world.write_config(&config);
    assert!(matches!(
        peers(&world.places, &args(&["add", "lab mini"])),
        Err(Failure::Usage(_))
    ));
    for (unheard, said) in [
        ("[2001:4860::8888]:7100", "is no LAN address"),
        ("[fd00::9]:7100", "is an IPv6 address"),
        ("[fe80::1]:7100", "is an IPv6 address"),
        ("[::ffff:10.0.0.8]:7100", "is an IPv6 address"),
    ] {
        let refused = match peers(&world.places, &args(&["add", unheard])) {
            Err(Failure::Usage(why)) => why,
            other => panic!("{unheard}: {other:?}"),
        };
        assert!(
            refused.contains(said) && refused.ends_with("nothing was written"),
            "{unheard}: {refused}"
        );
    }
    assert_eq!(hosts(), ["lab-mini:7101"], "a refusal writes nothing");
    ok(peers(&world.places, &args(&["add", "[::1]:7101"])));
    ok(peers(&world.places, &args(&["rm", "[::1]:7101"])));
    let removed = ok(peers(&world.places, &args(&["rm", "lab-mini"])));
    assert!(!removed.contains("init --rotate"), "{removed}");
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
fn a_leave_says_only_what_its_write_ended() {
    const UNLISTENS: &str = "stops listening";
    const UNTOLD: &str = "The issuer is not told";
    let by_hand = World::new("leave-said-by-hand");
    ok(peers(&by_hand.places, &args(&["add", "127.0.0.1:7101"])));
    let left = ok(leave(&by_hand.places, &[]));
    assert!(
        left.contains("(the book of peers)") && !left.contains(UNLISTENS) && !left.contains(UNTOLD),
        "{left}"
    );

    let (issuer, code, _serving) = issuing("leave-said-issuer");
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let port = closed_port();
    let opts = Opts {
        port: Some(port),
        ..Opts::default()
    };
    let member = World::new("leave-said-member");
    ok(join(&member.places, &opts, &code));
    std::fs::remove_file(member.places.state.join(KEY_FILE)).unwrap();
    let left = ok(leave(&member.places, &[]));
    assert!(
        !left.contains(UNLISTENS) && left.contains(UNTOLD),
        "a key gone before the leave: {left}"
    );
    ok(join(&member.places, &opts, &code));
    ok(peers(&member.places, &args(&["rm", &issuer_node])));
    let left = ok(leave_within(&member.places, HANG));
    assert!(
        !left.contains(UNLISTENS) && !left.contains(UNTOLD),
        "nothing listened here: {left}"
    );
    ok(join(&member.places, &opts, &code));
    let listening = Listener::serve(
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap(),
        &member.places.state,
    );
    ok(peers(&member.places, &args(&["rm", &issuer_node])));
    let left = ok(leave_within(&member.places, HANG));
    assert!(
        left.contains(UNLISTENS) && !left.contains(UNTOLD),
        "a book that names no issuer, its listener serving: {left}"
    );
    drop(listening);
    ok(join(&member.places, &opts, &code));
    member.write_config("[fleet]\nlisten = \"\"\n");
    let left = ok(leave(&member.places, &[]));
    assert!(
        left.contains("[fleet] listen") && !left.contains(UNLISTENS),
        "a blank listen is no listener: {left}"
    );
}

#[test]
fn a_join_to_an_issuer_past_the_lan_is_refused_by_its_kind() {
    let secret_bytes = [7; member::SECRET_LEN];
    let code = JoinCode {
        secret: secret_bytes,
        hint: None,
    }
    .text_at(("192.0.2.1", 7100));
    let secret = FleetSecret::new(&secret_bytes).unwrap();
    let registration = Registration {
        node: "ab".repeat(16),
        host: "lab-mini.invalid".to_owned(),
        port: 7100,
        version: "0.95.0".to_owned(),
        key: None,
        signed: None,
    };
    let me = aterm_net::fleet::NodeIdentity::from_seed(&registration.node, &[3; 32]).unwrap();
    let refused = register_with(
        &[("192.0.2.1".to_owned(), 7100)],
        (&secret, &me),
        (&Keyring::default(), None),
        &registration,
        &Vec::new,
    );
    assert!(
        matches!(&refused, Err(JoinFailed::Dial(Unreached::NotFleet(_)))),
        "{refused:?}"
    );
    let fresh = World::new("past-the-lan");
    let opts = Opts {
        host: Some("past-the-lan.invalid".to_owned()),
        ..Opts::default()
    };
    assert!(matches!(
        join(&fresh.places, &opts, &code),
        Err(Failure::Failed(_))
    ));
    assert!(!fresh.places.state.join(KEY_FILE).exists(), "no key");
    assert!(!fresh.places.config.exists(), "no [fleet]");
}

#[test]
fn a_first_join_its_issuer_never_answers_says_it_kept_the_node_key() {
    let port = closed_port();
    let code = JoinCode {
        secret: [7; member::SECRET_LEN],
        hint: None,
    }
    .text_at(("127.0.0.1", port));
    let fresh = World::new("unanswered-first");
    let opts = Opts {
        port: Some(port.wrapping_add(1).max(1)),
        ..Opts::default()
    };
    let refused = || match join(&fresh.places, &opts, &code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}"),
    };
    let first = refused();
    assert!(
        first.contains(
            "Nothing of the fleet was written here; this machine's node key (fleet-node, \
             fleet-node-key), minted for the join, is kept"
        ),
        "{first}"
    );
    assert!(fresh.places.state.join(NODE_KEY_FILE).exists());
    for file in [KEY_FILE, PEERS_FILE] {
        assert!(!fresh.places.state.join(file).exists(), "{file}");
    }
    let again = refused();
    assert!(
        again.contains("Nothing of the fleet was written here.") && !again.contains("minted"),
        "{again}"
    );
}

#[test]
fn every_reason_a_fleet_read_gives_fits_its_row() {
    use aterm_ctl::PeerFault;
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
            PeerFault::Unserved,
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
fn a_member_of_a_loopback_fleet_is_recorded_where_it_listens() {
    let (issuer, code, _serving) = issuing("loopback-host-issuer");
    let member = World::new("loopback-host-member");
    let port = closed_port();
    for opts in [
        Opts {
            port: Some(port),
            ..Opts::default()
        },
        Opts::default(),
    ] {
        let joined = ok(join(&member.places, &opts, &code));
        assert!(
            joined.contains(&format!("recorded this machine as 127.0.0.1:{port} ")),
            "{opts:?}: {joined}"
        );
    }
    let book = read_peers(&issuer.places.state).unwrap();
    assert_eq!(
        book.iter()
            .map(|peer| (peer.host.as_str(), peer.port))
            .collect::<Vec<_>>(),
        [("127.0.0.1", port)]
    );
}

#[test]
fn a_join_at_a_place_another_member_listens_writes_nothing_and_the_issuer_names_the_port() {
    let (issuer, code, _serving) = issuing("held-place-issuer");
    let holder = World::new("held-place-holder");
    let port = closed_port();
    let at = Opts {
        port: Some(port),
        ..Opts::default()
    };
    ok(join(&holder.places, &at, &code));
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let _listening = Listener::serve(listener, &holder.places.state);
    let holder_node = member::node_id(&holder.places.state).unwrap();
    let book = read_peers(&issuer.places.state).unwrap();

    let newcomer = World::new("held-place-newcomer");
    match with_peers_answering(|| join(&newcomer.places, &at, &code)) {
        Err(Failure::Failed(why)) => assert!(
            why.contains(&format!(
                "node {} of this fleet listens at 127.0.0.1:{port} now",
                &holder_node[..8]
            )) && why.contains("name a free port with `--port <n>`, and join again"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(read_key_file(&newcomer.places.state).is_err());
    assert_eq!(newcomer.config(), "", "nothing of the fleet was written");
    assert_eq!(read_peers(&issuer.places.state).unwrap(), book);
    assert!(
        ok(join(&holder.places, &at, &code)).contains(&format!("as 127.0.0.1:{port} ")),
        "its own listener there is no other node"
    );

    let newcomer_node = member::node_id(&newcomer.places.state).unwrap();
    member::update_peers(&issuer.places.state, |book| {
        member::record(
            book,
            Peer {
                node: Some(newcomer_node.clone()),
                key: None,
                key_from: None,
                host: "127.0.0.1".to_owned(),
                addr: Some(IpAddr::from([127, 0, 0, 1])),
                port,
                version: None,
                seen_ms: None,
                via: Via::Joined,
                fleet: None,
                signed: None,
            },
        )
    })
    .unwrap();
    let listed = ok(list_peers(&issuer.places, HANG));
    assert!(
        listed.contains(&format!(
            "node {} holds this place: `join --port <n>` on {}, or `aterm fleet peers rm {}`",
            &holder_node[..8],
            &newcomer_node[..8],
            &newcomer_node[..8]
        )),
        "{listed}"
    );
}

#[test]
fn a_join_at_another_machines_address_is_told_its_host_not_its_port() {
    let holder = "ab".repeat(16);
    let refused = |place: &str| {
        not_recorded(
            "127.0.0.1:7100",
            &JoinFailed::PlaceHeld {
                place: place.to_owned(),
                node: holder.clone(),
            },
            (Path::new("/state"), false),
        )
    };
    let elsewhere = refused("192.0.2.1:7100");
    assert!(
        elsewhere.contains("192.0.2.1 is no address of this machine")
            && elsewhere.contains("with `--host <h>`, and join again")
            && !elsewhere.contains("--port"),
        "{elsewhere}"
    );
    let here = refused("127.0.0.1:7100");
    assert!(
        here.contains("name a free port with `--port <n>`, and join again")
            && !here.contains("--host"),
        "{here}"
    );
    let unplaced = refused("no-such-mac.invalid:7100");
    assert!(
        unplaced.contains("`--port <n>` (or with `--host <h>`"),
        "{unplaced}"
    );
}

#[test]
fn only_one_machines_two_listeners_at_a_place_are_told_the_port() {
    let (gone, came) = ("aa".repeat(16), "bb".repeat(16));
    let at = |node: &str, host: &str| Peer {
        node: Some(node.to_owned()),
        addr: Some(IpAddr::from([192, 168, 1, 10])),
        via: Via::Joined,
        ..manual_peer(host.to_owned(), 7100)
    };
    let why = Unreached::OtherNode {
        at: "192.168.1.10:7100".parse().unwrap(),
        said: came.clone(),
        want: gone.clone(),
    };
    let rejoin = format!(
        "node {} is here: `aterm fleet peers rm {}` if it left, or rejoin it",
        &came[..8],
        &gone[..8]
    );
    for (gone_host, came_host) in [
        ("x-laptop.local", "y-laptop.local"),
        ("192.168.1.10", "192.168.1.10"),
    ] {
        let listed = [at(&gone, gone_host), at(&came, came_host)];
        assert_eq!(
            unreached_words(&listed[0], &why, &listed, (None, None)),
            rejoin,
            "a readdressed member: {gone_host}"
        );
    }
    let listed = [at(&gone, "lab-mini.local"), at(&came, "LAB-MINI.local")];
    assert!(
        unreached_words(&listed[0], &why, &listed, (None, None))
            .contains("`join --port <n>` on aaaaaaaa"),
        "one name's two listeners"
    );
}

#[test]
fn a_place_this_machines_own_node_answers_at_is_named_as_this_machine() {
    let (gone, own) = ("aa".repeat(16), "cc".repeat(16));
    let copy = |host: &str| Peer {
        node: Some(gone.clone()),
        addr: host.parse().ok(),
        via: Via::IssuerBook,
        ..manual_peer(host.to_owned(), 52509)
    };
    let at = |ip: [u8; 4]| Unreached::OtherNode {
        at: SocketAddr::from((ip, 52509)),
        said: own.clone(),
        want: gone.clone(),
    };
    let named = |host: &str, ip, own| unreached_words(&copy(host), &at(ip), &[], (own, None));
    assert_eq!(
        named("127.0.0.1", [127, 0, 0, 1], Some(own.as_str())),
        "this machine holds this place: `join --port <n>` on aaaaaaaa, or the issuer's `peers rm \
         aaaaaaaa`"
    );
    assert_eq!(
        named("192.168.1.10", [127, 0, 0, 1], Some(own.as_str())),
        "this machine is here: `join` on aaaaaaaa, or the issuer's `peers rm aaaaaaaa`"
    );
    let wide = "fd12:3456:789a:bcde:f012:3456:789a:bcde";
    let room = aterm_ctl::UNANSWERED_ROOM - shown_name(&copy(wide)).chars().count();
    let beside = named(wide, [127, 0, 0, 1], Some(own.as_str()));
    assert!(
        beside.starts_with("this machine is here:") && beside.chars().count() <= room,
        "{beside} beside an IPv6 place, in {room}"
    );
    for (ip, own) in [([127, 0, 0, 1], None), ([192, 0, 2, 1], Some(own.as_str()))] {
        assert_eq!(
            named("192.168.1.10", ip, own),
            "node cccccccc is here: the issuer's `peers rm aaaaaaaa` if it left, or rejoin it",
            "{ip:?}: no proof here of this machine"
        );
    }
}

#[test]
fn another_secret_on_the_issuer_is_told_by_when_its_node_was_seen() {
    let at = SocketAddr::from(([192, 168, 100, 200], 7100));
    let why = Unreached::Handshake(at, FleetError::ListenerWrongSecret);
    let seen = |seen_ms| Peer {
        node: Some("aa".repeat(16)),
        seen_ms,
        via: Via::Joined,
        ..manual_peer("192.168.100.200".to_owned(), 7100)
    };
    for (seen_ms, says) in [
        (
            Some(1_000),
            "it joined before a rotate: it joins again with the new code",
        ),
        (
            Some(3_000),
            "it left or joined another fleet: it joins again with this code",
        ),
        (
            None,
            "it holds another fleet's: it joins this fleet with this code",
        ),
    ] {
        let text = unreached_words(&seen(seen_ms), &why, &[], (None, Some(2_000)));
        assert_eq!(text, format!("another secret at {at}: {says}"));
        assert!(
            text.chars().count() <= aterm_ctl::UNANSWERED_REASON_CHARS,
            "{text}"
        );
    }
    assert_eq!(
        unreached_words(&seen(Some(3_000)), &why, &[], (None, None)),
        why.to_string(),
        "a member's"
    );
}

#[test]
fn a_displaced_nodes_row_names_it_and_its_remedy_within_a_reason() {
    let (gone, came) = ("17df7cfa".repeat(4), "bcff7226".repeat(4));
    for (at, gone_host) in [
        ("192.168.100.200:65535", "lab-mini.local"),
        (
            "[fd12:3456:789a:bcde:f012:3456:789a:bcde]:65535",
            "lab-mini.local",
        ),
        (
            "[fd12:3456:789a:bcde:f012:3456:789a:bcde]:65535",
            "fd12:3456:789a:bcde:f012:3456:789a:bcde",
        ),
    ] {
        displaced_within_a_reason(at.parse().unwrap(), gone_host, (&gone, &came));
    }
}

fn displaced_within_a_reason(
    at: std::net::SocketAddr,
    gone_host: &str,
    (gone, came): (&str, &str),
) {
    let why = || Unreached::OtherNode {
        at,
        said: came.to_owned(),
        want: gone.to_owned(),
    };
    // An issuer listens at no IPv6 place but the loopback's.
    let vias: &[Via] = if at.is_ipv4() {
        &[Via::Joined, Via::IssuerBook, Via::Issuer]
    } else {
        &[Via::Joined, Via::IssuerBook]
    };
    for &via in vias {
        for came_host in [gone_host, "x-laptop.local"] {
            let peer = |node: &str, host: &str| Peer {
                node: Some(node.to_owned()),
                addr: Some(at.ip()),
                via,
                ..manual_peer(host.to_owned(), at.port())
            };
            let listed = [peer(gone, gone_host), peer(came, came_host)];
            let text = unreached_words(&listed[0], &why(), &listed, (None, None));
            let label = shown_name(&listed[0]);
            // An address label stays whole, and the reason has the rest of the line.
            let room = if label.parse::<IpAddr>().is_ok() || label.parse::<SocketAddr>().is_ok() {
                aterm_ctl::UNANSWERED_ROOM - label.chars().count()
            } else {
                aterm_ctl::UNANSWERED_REASON_CHARS
            };
            let rm = if via == Via::Issuer { gone } else { &gone[..8] };
            assert!(
                text.chars().count() <= room
                    && text.contains(&format!("peers rm {rm}`"))
                    && text.contains(&came[..8])
                    && (via != Via::IssuerBook || text.contains("the issuer's"))
                    && (came_host != "lab-mini.local"
                        || via == Via::Issuer
                        || text.contains("`join --port <n>`")),
                "{} > {room} beside {label}, or a part missing: {text}",
                text.chars().count(),
            );
            let row = peer_survey(&listed[0], Err(why()), (&listed, None, None));
            assert_eq!(row.node.as_deref(), Some(gone));
        }
    }
}

#[test]
fn a_drive_to_a_copied_node_another_answers_for_names_the_issuers_rm() {
    const DRIVES: &[FleetOp] = &[
        FleetOp::Survey,
        FleetOp::Book,
        FleetOp::Join,
        FleetOp::Drive,
    ];
    const READS: &[FleetOp] = &[FleetOp::Survey, FleetOp::Book, FleetOp::Join];
    for (n, serves) in [DRIVES, READS].into_iter().enumerate() {
        let issuer = World::new(&format!("drive-copy-issuer-{n}"));
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        ok(init(
            &issuer.places,
            &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
        ));
        let code = vault_code(&issuer);
        let _serving = Listener::serving(listener, &issuer.places.state, serves);
        let member = World::new(&format!("drive-copy-member-{n}"));
        let opts = Opts {
            port: Some(closed_port()),
            ..Opts::default()
        };
        ok(join(&member.places, &opts, &code));
        let issuer_node = member::node_id(&issuer.places.state).unwrap();
        let gone = "cd".repeat(16);
        let copy = vec![Peer {
            node: Some(gone.clone()),
            via: Via::IssuerBook,
            ..manual_peer("127.0.0.1".to_owned(), port)
        }];
        member::hold_book(&member.places.state)
            .unwrap()
            .keep_issuer_book(Some((&issuer_node, &copy)))
            .unwrap();
        let refused = drive_fleet_in(&member.places, &gone, HANG)
            .err()
            .expect("another node answers there");
        assert!(
            refused.why.ends_with(&format!(
                "node {} holds this place: `join --port <n>` on {}, or the issuer's `peers rm {}`",
                &issuer_node[..8],
                &gone[..8],
                &gone[..8]
            )),
            "{serves:?}: {refused:?}"
        );
    }
}

#[test]
fn a_drive_its_peer_serves_none_of_is_one_its_grant_is_not_given() {
    let (issuer, code, serving) = issuing("ungranted-issuer");
    let member = World::new("ungranted-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    let refused = drive_fleet_in(&member.places, &issuer_node, HANG)
        .err()
        .expect("it serves no drive");
    assert!(
        refused.ungranted && refused.why.contains("lets no one drive it"),
        "{refused:?}"
    );
    drop(serving);
    let refused = drive_fleet_in(&member.places, &issuer_node, HANG)
        .err()
        .expect("nothing listens there");
    assert!(!refused.ungranted, "{refused:?}");
}

#[test]
fn a_member_reaches_a_member_that_joined_after_its_last_read() {
    let (_issuer, code, _serving) = issuing("late-member-issuer");
    let opts = || Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    let early = World::new("late-member-early");
    ok(join(&early.places, &opts(), &code));
    let joined_after = |tag: &str| {
        let late = World::new(tag);
        ok(join(&late.places, &opts(), &code));
        let node = member::node_id(&late.places.state).unwrap();
        assert!(
            !member::bound_peers(&early.places.state)
                .iter()
                .any(|peer| peer.node.as_deref() == Some(node.as_str())),
            "no read since its join"
        );
        node
    };
    let late = joined_after("late-member-late");
    let refused = with_issuer_serving(|| drive_fleet_in(&early.places, &late, HANG))
        .err()
        .expect("nothing listens at its place");
    assert!(
        refused.left.is_none() && !refused.why.contains("no Mac this machine's book records"),
        "{refused:?}"
    );
    assert!(
        member::bound_peers(&early.places.state)
            .iter()
            .any(|peer| peer.node.as_deref() == Some(late.as_str())),
        "the drive kept the book it read"
    );
    let later = joined_after("late-member-later");
    let read = with_issuer_serving(|| {
        survey_fleet_in(
            &early.places,
            &aterm_ctl::FleetAsk {
                host: Some(&later),
                within: 2 * HANG,
            },
        )
    })
    .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(
        read.peers
            .iter()
            .map(|row| row.node.as_deref())
            .collect::<Vec<_>>(),
        [Some(later.as_str())],
        "{:?}",
        read.peers
    );
}

#[test]
fn a_survey_reads_its_issuers_book_again_within_its_own_deadline() {
    let (_issuer, code, serving) = issuing("survey-within-issuer");
    let member = World::new("survey-within-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let silent = serving.take();
    silent.set_nonblocking(true).unwrap();
    let elsewhere = format!("127.0.0.1:{}", closed_port());
    let read = survey_fleet_in(
        &member.places,
        &aterm_ctl::FleetAsk {
            host: Some(&elsewhere),
            within: Duration::ZERO,
        },
    )
    .unwrap_or_else(|why| panic!("{why}"));
    assert!(
        read.peers.iter().all(|row| row.answer.is_err()),
        "{:?}",
        read.peers
    );
    assert!(
        matches!(silent.accept(), Err(e) if e.kind() == io::ErrorKind::WouldBlock),
        "a survey whose deadline is spent dialled the issuer"
    );
}

#[test]
fn a_member_past_a_silent_issuer_spends_one_deadline_and_says_its_issuer_did_not_answer() {
    let (_issuer, code, serving) = issuing("silent-issuer");
    let member = World::new("silent-issuer-member");
    let opts = || Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts(), &code));
    let own = member::node_id(&member.places.state).unwrap();
    match allow(&member.places, &args(&["drive", "--from", &own[..8]])) {
        Err(Failure::Failed(why)) => assert!(why.contains("is this machine itself"), "{why}"),
        other => panic!("{other:?}"),
    }
    let late = World::new("silent-issuer-late");
    ok(join(&late.places, &opts(), &code));
    let silent = serving.take();
    // Each read of the issuer below connects within the hang detector, its read's bound, and the
    // handshake the issuer never sends ends it there at once ([`member::HeldSilent`]), save where
    // a read's real bound is what is checked (the answering host).
    let issuer_held = || member::HeldSilent::at(silent.local_addr().unwrap());
    // The read is given twice the drive's deadline, which caps it, so the refusal names the
    // drive's.
    let refused = {
        let _held = issuer_held();
        with_issuer_read_within(2 * HANG, || {
            drive_fleet_in(&member.places, "zz-unbooked", HANG)
        })
    }
    .err()
    .expect("no book names it");
    assert!(
        refused.again
            && refused.unserved
            && refused.why.starts_with(&format!(
                "its issuer did not answer within {:.1}s, so no book here says which Mac \
                 `zz-unbooked` is",
                HANG.as_secs_f64()
            )),
        "{refused:?}"
    );
    let allowed = {
        let _held = issuer_held();
        with_issuer_read_within(HANG, || {
            allow(&member.places, &args(&["drive", "--from", "zz-unbooked"]))
        })
    };
    match allowed {
        Err(Failure::Failed(why)) => assert!(
            why.starts_with(
                "`zz-unbooked` names no peer this machine knows, and its issuer did not answer \
                 within"
            ) && why.contains("try again once it answers"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    let survey = |word: &str, within| {
        survey_fleet_in(
            &member.places,
            &aterm_ctl::FleetAsk {
                host: Some(word),
                within,
            },
        )
        .unwrap_or_else(|why| panic!("{why}"))
    };
    let host = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let word = host.local_addr().unwrap().to_string();
    let hanging = std::thread::spawn(move || drop(host.accept().unwrap()));
    // The issuer's read spends none of the deadline here, so a host dial given that read's bound
    // in place of the survey's own deadline would end before it connects.
    let read = with_issuer_read_within(Duration::ZERO, || survey(&word, 2 * HANG));
    let said = read.peers[0].answer.as_ref().expect_err("it hung up");
    assert_eq!(
        said,
        &format!(
            "{word} hung up mid-handshake: its fleet listener restarting, or no aterm; try again"
        ),
        "the host dial's whole reason, which leaves no room for a lead"
    );
    hanging.join().unwrap();
    let note = read.note.as_deref().unwrap_or_default();
    assert!(
        note.starts_with(&format!(
            "its issuer did not answer within 0.0s, so no book here names `{word}`, and it was \
             read as a host: try again once the issuer answers"
        )),
        "{note}"
    );
    let closed = format!("127.0.0.1:{}", closed_port());
    let read = {
        let _held = issuer_held();
        with_issuer_read_within(2 * HANG, || survey(&closed, 2 * HANG))
    };
    let said = read.peers[0].answer.as_ref().expect_err("nothing listens");
    assert_eq!(
        said,
        &format!(
            "issuer silent, as a host: nothing listens on {closed}: no aterm running there, \
             or no [fleet] listen"
        ),
        "the issuer's silence first, then the dial's whole reason"
    );
    assert!(
        said.chars().count() + read.peers[0].label.chars().count() <= aterm_ctl::UNANSWERED_ROOM,
        "the row prints it uncut: {said}"
    );
    let answers = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let word = answers.local_addr().unwrap().to_string();
    let _answers = Listener::serve(answers, &late.places.state);
    let read = survey(&word, HANG);
    assert!(
        read.peers[0].answer.is_ok(),
        "a host no book names is read in the rest of the deadline: {:?}",
        read.peers
    );
    let note = read.note.unwrap_or_default();
    assert!(
        note.contains(&format!(
            "did not answer within 3.0s, so no book here names `{word}`, and it was read as a \
             host"
        )),
        "a row that answered says its issuer did not: {note:?}"
    );
    let late_node = member::node_id(&late.places.state).unwrap();
    let _held = issuer_held();
    for word in [late_node.as_str(), &late_node[..8], "127.0.0.1"] {
        // The read is given the survey's whole deadline, and half of it caps the read.
        let refused = with_issuer_read_within(2 * HANG, || {
            survey_fleet_in(
                &member.places,
                &aterm_ctl::FleetAsk {
                    host: Some(word),
                    within: 2 * HANG,
                },
            )
        })
        .map(|read| read.peers.len());
        let lead = format!(
            "`{word}` names no peer this machine knows, and its issuer did not answer within {:.1}s",
            HANG.as_secs_f64()
        );
        assert!(
            matches!(&refused, Err(refused) if refused.unserved
                && (word == "127.0.0.1" || refused.why.starts_with(&lead))),
            "a node's word no book binds is never read as a host: {refused:?}"
        );
        let driven =
            with_issuer_read_within(2 * HANG, || drive_fleet_in(&member.places, word, HANG))
                .err()
                .expect("no book here binds it");
        assert!(
            driven.unserved,
            "a drive's refusal for the issuer's silence exits as the survey's: {driven:?}"
        );
    }
}

#[test]
fn a_grant_its_silent_issuer_could_not_confirm_says_its_key_came_from_the_copy() {
    let (_issuer, code, serving) = issuing("unconfirmed-issuer");
    let driver = World::new("unconfirmed-driver");
    let granter = World::new("unconfirmed-granter");
    let opts = || Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&driver.places, &opts(), &code));
    ok(join(&granter.places, &opts(), &code));
    ok(list_peers(&granter.places, HANG));
    let node = member::node_id(&driver.places.state).unwrap();
    let _silent = serving.take();
    let granted = ok(allow(&granter.places, &args(&["drive", "--from", &node])));
    assert!(
        granted.contains(&format!(
            "  key        node {} is bound here only by this machine's copy of its issuer's \
             book, which the issuer could not confirm just now (its issuer did not answer within",
            &node[..8]
        )),
        "{granted}"
    );
    assert_eq!(fleet_table(&granter.config()).unwrap().drive_from.len(), 1);
}

#[test]
fn a_drive_that_meets_no_listener_exits_the_same_at_its_issuer_and_at_its_peer() {
    let (_issuer, code, serving) = issuing("closed-issuer");
    let member = World::new("closed-issuer-member");
    let late = World::new("closed-issuer-late");
    let opts = || Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts(), &code));
    ok(join(&late.places, &opts(), &code));
    let node = member::node_id(&late.places.state).unwrap();
    let at_peer = with_issuer_serving(|| drive_fleet_in(&member.places, &node, HANG))
        .err()
        .expect("nothing listens at its place");
    drop(serving.take());
    let at_issuer = drive_fleet_in(&member.places, "zz-unbooked", HANG)
        .err()
        .expect("nothing listens at the issuer's");
    assert!(
        at_issuer.why.contains("try again once it answers"),
        "{at_issuer:?}"
    );
    for refused in [&at_peer, &at_issuer] {
        assert!(refused.again && !refused.unserved, "{refused:?}");
    }
}

#[test]
fn an_issuer_that_answers_with_no_book_is_told_once() {
    let (issuer, code, serving) = issuing("bookless-issuer");
    let member = World::new("bookless-issuer-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let _older = Listener::serving(
        serving.take(),
        &issuer.places.state,
        &[FleetOp::Survey, FleetOp::Join],
    );
    let refused = with_issuer_serving(|| drive_fleet_in(&member.places, "zz-unbooked", HANG))
        .err()
        .expect("no book names it");
    assert!(
        !refused.again
            && refused
                .why
                .contains("runs an older aterm, which shares no book")
            && !refused.why.contains("try again"),
        "{refused:?}"
    );
    let elsewhere = format!("127.0.0.1:{}", closed_port());
    let read = with_issuer_serving(|| {
        survey_fleet_in(
            &member.places,
            &aterm_ctl::FleetAsk {
                host: Some(&elsewhere),
                within: 2 * HANG,
            },
        )
    })
    .unwrap_or_else(|why| panic!("{why}"));
    let said = read.peers[0].answer.as_ref().expect_err("nothing listens");
    assert!(
        said.starts_with("no issuer's book, as a host: nothing listens on ")
            && !said.contains("try again"),
        "{said}"
    );
    let note = read.note.as_deref().unwrap_or_default();
    assert!(
        note.contains("runs an older aterm")
            && note.contains("it was read as a host")
            && !note.contains("try again"),
        "{note}"
    );
    match with_issuer_serving(|| allow(&member.places, &args(&["drive", "--from", "zz-unbooked"])))
    {
        Err(Failure::Failed(why)) => assert!(
            why.contains("runs an older aterm") && !why.contains("try again"),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_join_that_would_take_the_issuers_own_address_writes_nothing() {
    let (_issuer, code, _serving) = issuing("same-place-issuer");
    let member = World::new("same-place-member");
    for opts in [
        Opts::default(),
        Opts {
            host: Some("localhost".to_owned()),
            ..Opts::default()
        },
    ] {
        match join(&member.places, &opts, &code) {
            Err(Failure::Usage(why)) => assert!(
                why.contains("the place of the member it dials")
                    && why.contains("so name one with `--port <n>`")
                    && !why.contains("own port"),
                "{opts:?}: {why}"
            ),
            other => panic!("{opts:?}: {other:?}"),
        }
    }
    let issuers = hinted_port(&code);
    match join(
        &member.places,
        &Opts {
            port: Some(issuers),
            ..Opts::default()
        },
        &code,
    ) {
        Err(Failure::Usage(why)) => assert!(
            why.contains(&format!(
                "`--port {issuers}` is that member's own port there, so name another with \
                 `--port <n>`"
            )),
            "{why}"
        ),
        other => panic!("{other:?}"),
    }
    assert!(
        !member.places.state.exists(),
        "nothing of the fleet was written"
    );
    assert_eq!(member.config(), "");
    let port = closed_port();
    let joined = ok(join(
        &member.places,
        &Opts {
            port: Some(port),
            ..Opts::default()
        },
        &code,
    ));
    assert!(
        joined.contains(&format!("as 127.0.0.1:{port} ")),
        "{joined}"
    );
}

#[test]
fn a_place_no_read_takes_as_a_peer_is_refused_at_its_add_and_skipped_when_booked() {
    let (issuer, _code, _serving) = issuing("unread-place");
    let state = &issuer.places.state;
    let port = fleet_table(&issuer.config())
        .unwrap()
        .listen_port()
        .unwrap();
    let book = read_peers(state).unwrap();
    let own = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    for place in ["8.8.8.8", "100.64.1.2:7100", "[2001:db8::1]:7100"]
        .into_iter()
        .chain(own.iter().map(String::as_str))
    {
        match peers(&issuer.places, &args(&["add", place])) {
            Err(Failure::Usage(why)) => {
                assert!(why.contains("nothing was written"), "{place}: {why}")
            }
            other => panic!("{place}: {other:?}"),
        }
    }
    assert_eq!(read_peers(state).unwrap(), book, "nothing was written");
    let other = format!("127.0.0.1:{}", closed_port());
    assert!(ok(peers(&issuer.places, &args(&["add", &other]))).contains("added"));

    member::update_peers(state, |book| {
        member::record(book, manual_peer("127.0.0.1".to_owned(), port))
    })
    .unwrap();
    let listed = ok(list_peers(&issuer.places, HANG));
    assert!(
        listed.contains(&format!(
            "this machine itself, no peer: `aterm fleet peers rm 127.0.0.1:{port}`"
        )),
        "{listed}"
    );
    let place = format!("127.0.0.1:{port}");
    ok(peers(&issuer.places, &args(&["rm", &place])));
    let config = issuer.config();
    issuer.write_config(&format!("{config}\n[[fleet.peers]]\nhost = \"{place}\"\n"));
    let edit = format!(
        "aterm.toml's `[[fleet.peers]] host = \"{place}\"` names it: take that entry out of {}",
        issuer.places.config.display()
    );
    let listed = ok(list_peers(&issuer.places, HANG));
    assert!(
        listed.contains(&format!("this machine itself, no peer: {edit}")),
        "{listed}"
    );
    match peers(&issuer.places, &args(&["rm", &place])) {
        Err(Failure::Failed(why)) => assert!(why.ends_with(&edit), "{why}"),
        other => panic!("{other:?}"),
    }
    issuer.write_config(&config);
    assert!(
        !ok(list_peers(&issuer.places, HANG)).contains("this machine itself"),
        "the edit it names takes it out"
    );
    match peers(&issuer.places, &args(&["rm", &place])) {
        Err(Failure::Failed(why)) => assert!(
            why.starts_with(&format!("`{place}` is this machine itself (node ")),
            "nothing listed there: {why}"
        ),
        other => panic!("{other:?}"),
    }
    let whole = aterm_ctl::FleetAsk {
        host: None,
        within: HANG,
    };
    let read = survey_fleet_in(&issuer.places, &whole).unwrap_or_else(|why| panic!("{why}"));
    let labels: Vec<&str> = read.peers.iter().map(|peer| peer.label.as_str()).collect();
    assert_eq!(
        labels,
        [other.as_str()],
        "this machine is never its own peer"
    );
}

#[test]
fn a_listen_place_another_node_answers_at_is_one_this_listener_is_not_on() {
    let (issuer, code, _serving) = issuing("held-listen-issuer");
    let member = World::new("held-listen-member");
    let port = closed_port();
    let opts = Opts {
        port: Some(port),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let head = |listed: &str| listed.lines().next().unwrap_or_default().to_owned();
    let list = || with_peers_answering(|| ok(peers(&member.places, &[])));
    let unserved = list();
    assert!(
        head(&unserved).ends_with(&format!(
            "([fleet] listen is 127.0.0.1:{port} and allow is facts, but nothing listens on \
             127.0.0.1:{port} just now: no aterm runs here (no control socket here takes a \
             connection): one started here reads [fleet] at launch and listens there)"
        )),
        "{unserved}"
    );
    // Bound and never accepted: the kernel takes the connection, and no handshake comes.
    let held = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap();
    let held_silent = member::HeldSilent::at(held.local_addr().unwrap());
    let silent = head(&list());
    assert!(
        silent.ends_with(&format!(
            "127.0.0.1:{port} takes a connection but finishes no fleet handshake just now: \
             another program holds that port, so no aterm runs here, and one started here reads \
             fleet=retrying until that program lets it go; `aterm fleet join --port <n>` moves \
             it)"
        )),
        "{silent}"
    );
    // The aterm here listens elsewhere: the header quotes its own word.
    let listening = head(&with_aterm_here(
        || Here::Answers(Some("listening".to_owned())),
        list,
    ));
    assert!(
        listening.contains("though the aterm running here reads fleet=listening")
            && !listening.contains("fleet=retrying"),
        "{listening}"
    );
    drop(held_silent);
    let serving = Listener::serve(held, &member.places.state);
    let free = list();
    assert!(
        head(&free).ends_with(&format!("(it listens on 127.0.0.1:{port}, serving facts)")),
        "{free}"
    );
    drop(serving);
    let taken = fleet_table(&issuer.config())
        .unwrap()
        .listen_port()
        .unwrap();
    let node = member::node_id(&issuer.places.state).unwrap();
    let config = member.config();
    // Linux refuses a wildcard's bind beside another node's listener; macOS binds it, and its
    // own node proves itself nowhere this read can tell.
    for (place, held) in [
        (format!("127.0.0.1:{taken}"), true),
        (format!("0.0.0.0:{taken}"), cfg!(target_os = "linux")),
    ] {
        member.write_config(&config.replace(&format!("127.0.0.1:{port}"), &place));
        let listed = list();
        if held {
            let said = format!(
                "(its listener is not on {place}: node {} answers there; `aterm fleet join \
                 --port <n>` moves it)",
                &node[..8]
            );
            assert!(head(&listed).ends_with(&said), "{listed}");
        } else {
            let said = format!(
                "([fleet] listen is {place} and allow is facts, but this machine's node did not \
                 prove itself there just now (node {} answers at 127.0.0.1:{taken})",
                &node[..8]
            );
            assert!(head(&listed).contains(&said), "{listed}");
        }
    }
}

#[test]
fn a_listen_that_is_no_ip_and_port_is_named_as_that() {
    let (issuer, _code, _serving) = issuing("listen-unparsed");
    let config = issuer.config();
    let listen = fleet_table(&config).unwrap().listen.unwrap();
    issuer.write_config(&config.replace(&listen, "localhost:7100"));
    let listed = ok(peers(&issuer.places, &[]));
    let head = listed.lines().next().unwrap_or_default();
    assert!(
        head.contains("([fleet] listen is localhost:7100, no numeric IP and port, so no aterm")
            && !head.contains("node key"),
        "{listed}"
    );
}

#[test]
fn the_peers_listing_cuts_a_host_to_its_column_and_keeps_an_address_whole() {
    let world = World::new("peers-columns");
    member::ensure_private_dir(&world.places.state).unwrap();
    let wide = Peer {
        node: Some("ab".repeat(16)),
        addr: Some("fd12:3456:789a:bcde:f012:3456:789a:bcde".parse().unwrap()),
        version: Some("0.136.0+g6e235f5254".to_owned()),
        via: Via::Joined,
        ..manual_peer("a-host-name-longer-than-its-column.local".to_owned(), 7100)
    };
    let book = format!("{}\n", wide.line());
    write_private(&world.places.state, PEERS_FILE, book.as_bytes()).unwrap();
    let listed = ok(peers(&world.places, &[]));
    assert!(
        listed.contains(
            "a-host-name-longer-tha…  [fd12:3456:789a:bcde:f012:3456:789a:bcde]:7100  \
             0.136.0+g6e235f5254  "
        ),
        "{listed}"
    );
}

#[test]
fn peers_beside_a_key_it_cannot_use_refuses_saying_how_it_reads_again() {
    let (_issuer, code, _serving) = issuing("unusable-issuer");
    let member = World::new("unusable-member");
    let opts = Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let link = member.places.config.with_file_name("fleet.key.copy");
    std::fs::hard_link(member.places.state.join(KEY_FILE), &link).unwrap();
    let refused = peers(&member.places, &[]);
    assert!(
        matches!(&refused, Err(Failure::Failed(why)) if why.contains("no peer was checked") && why.contains("another hard link") && why.contains("remove that link")),
        "{refused:?}"
    );
    let whole = aterm_ctl::FleetAsk {
        host: None,
        within: HANG,
    };
    let unread = survey_fleet_in(&member.places, &whole).expect_err("no key it can use");
    assert!(unread.why.contains("remove that link"), "{unread}");
    let rejoined = ok(join(&member.places, &opts, &code));
    assert!(
        rejoined.contains("in place of one it could not use")
            && rejoined.contains("that other link still holds a copy")
            && !rejoined.contains("in place of the one it held"),
        "{rejoined}"
    );
    assert!(link.exists(), "a join never looks for the other link");
    std::fs::remove_file(&link).unwrap();
    let listed = ok(peers(&member.places, &[]));
    assert!(
        listed.contains(" issuer "),
        "the book kept its issuer: {listed}"
    );
    write_private(&member.places.state, KEY_FILE, b"not a secret\n").unwrap();
    let refused = match peers(&member.places, &[]) {
        Err(Failure::Failed(why)) => why,
        listed => panic!("{listed:?} should be refused"),
    };
    assert!(
        refused.ends_with(": `aterm fleet join` replaces it") && !refused.contains("leave"),
        "one remedy, named once: {refused}"
    );
    let replaced = ok(join(&member.places, &opts, &code));
    assert!(
        replaced.contains("in place of a key file it could not use"),
        "{replaced}"
    );
    assert!(ok(peers(&member.places, &[])).contains(" issuer "));
}

#[test]
fn every_reason_a_key_gives_fits_a_survey_row_and_names_its_act() {
    use aterm_net::fleet::KeyFileError as E;
    use aterm_uds::private_file::{ACL_OPEN, ANOTHER_LINK, DIR_OPEN, FILE_OPEN};
    let reasons = [
        (E::Missing, "aterm fleet join"),
        (E::NotPrivate(ANOTHER_LINK), "remove that link"),
        (E::NotPrivate(FILE_OPEN), "`chmod 600`"),
        (E::NotPrivate(DIR_OPEN), "`chmod 700`"),
        (E::NotPrivate(ACL_OPEN), "`chmod -N`"),
        (E::NotPrivate("it is another user's"), "run this again"),
        (
            E::NotPrivate("the directory is another user's"),
            "run this again",
        ),
        (E::Symlink, "run this again"),
        (
            E::Malformed("not 40 to 64 hex digits on one line"),
            "`aterm fleet ",
        ),
        (E::Malformed("longer than one secret"), "`aterm fleet "),
        (
            E::Io(io::Error::from_raw_os_error(libc::EACCES)),
            "run this again",
        ),
    ];
    let member = World::new("key-reasons-member");
    let issuer = World::new("key-reasons-issuer");
    member::ensure_private_dir(&issuer.places.state).unwrap();
    write_private(&issuer.places.state, CODE_FILE, b"code\n").unwrap();
    for (error, act) in &reasons {
        for places in [&member.places, &issuer.places] {
            let text = no_key(places, error);
            assert!(
                text.chars().count() <= aterm_ctl::UNANSWERED_REASON_CHARS && text.contains(act),
                "{} > {}, or no {act}: {text}",
                text.chars().count(),
                aterm_ctl::UNANSWERED_REASON_CHARS
            );
        }
    }
    let malformed = E::Malformed("not 40 to 64 hex digits on one line");
    assert!(no_key(&member.places, &malformed).ends_with("`aterm fleet join` replaces it"));
    assert!(no_key(&issuer.places, &malformed).ends_with("`aterm fleet leave`, `init` again"));
    member::ensure_private_dir(&member.places.state).unwrap();
    let node = "ab".repeat(16);
    let older = Peer {
        key: Some(NodeIdentity::from_seed(&node, &[7; 32]).unwrap().key()),
        node: Some(node),
        via: Via::Issuer,
        ..manual_peer("127.0.0.1".to_owned(), 7100)
    };
    write_private(
        &member.places.state,
        PEERS_FILE,
        format!("{}\n", older.line()).as_bytes(),
    )
    .unwrap();
    assert!(
        no_key(&member.places, &malformed).ends_with("`aterm fleet leave`, join again"),
        "a join refuses beside an older book's issuer"
    );
}

#[test]
fn a_drive_that_meets_a_listener_between_generations_is_asked_again() {
    let at = std::net::SocketAddr::from(([127, 0, 0, 1], 7100));
    for again in [
        Unreached::Handshake(at, FleetError::Tls(io::ErrorKind::BrokenPipe.into())),
        Unreached::Handshake(at, FleetError::Io(io::ErrorKind::UnexpectedEof.into())),
        Unreached::Handshake(at, FleetError::AddressBusy),
        Unreached::Connect(at, io::ErrorKind::ConnectionRefused.into()),
        Unreached::Connect(at, io::Error::from_raw_os_error(libc::ECONNRESET)),
        Unreached::Connect(at, io::ErrorKind::ConnectionAborted.into()),
    ] {
        assert!(drive_again(&again), "{again:?}");
    }
    for once in [
        Unreached::Handshake(at, FleetError::ListenerWrongSecret),
        Unreached::Handshake(at, FleetError::UnknownOp("drive".to_owned())),
        Unreached::Handshake(at, FleetError::Tls(io::ErrorKind::InvalidData.into())),
    ] {
        assert!(!drive_again(&once), "{once:?}");
    }
}

#[test]
fn a_failed_join_is_advised_by_its_kind() {
    let at = std::net::SocketAddr::from(([192, 168, 1, 23], 7100));
    let dial =
        |errno| JoinFailed::Dial(Unreached::Connect(at, io::Error::from_raw_os_error(errno)));
    let older = "this machine is no fleet's issuer: it holds no join code, so it records no \
                 members (join at the machine whose `aterm fleet init` wrote the code)";
    let cases = [
        (
            JoinFailed::Registration(member::NOT_THE_ISSUER.to_owned()),
            Advice::NoIssuer,
        ),
        (JoinFailed::Registration(older.to_owned()), Advice::NoIssuer),
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
            JoinFailed::Dial(Unreached::Handshake(at, FleetError::ListenerWrongSecret)),
            Advice::OtherSecret,
        ),
        (
            JoinFailed::Registration("ERR the book is full".to_owned()),
            Advice::Answered,
        ),
        (
            JoinFailed::Registration(format!(
                "{}, and the issuer does not join itself",
                member::OWN_NODE
            )),
            Advice::IssuersNode,
        ),
        (
            JoinFailed::Registration(member::MOVED_ON.to_owned()),
            Advice::MovedOn,
        ),
        (
            JoinFailed::Dial(Unreached::Handshake(at, FleetError::AddressBusy)),
            Advice::Busy,
        ),
        (
            JoinFailed::Unanswered("the issuer did not answer within 2.9s".to_owned()),
            Advice::Unanswered,
        ),
        (
            JoinFailed::Unanswered("could not bound the registration: bad fd".to_owned()),
            Advice::Unanswered,
        ),
    ];
    for (failed, advice) in cases {
        assert_eq!(failed.advice(), advice, "{failed:?}");
    }
    for unanswered in [
        JoinFailed::Unanswered("the issuer did not answer within 2.9s".to_owned()),
        JoinFailed::Dial(Unreached::Handshake(at, FleetError::AddressBusy)),
    ] {
        let said = not_recorded(
            "192.168.1.23:7100",
            &unanswered,
            (Path::new("/state"), false),
        );
        assert!(!said.contains("The issuer answered"), "{said}");
    }
    let member_said = |words: &str| {
        not_recorded(
            "127.0.0.1:49201",
            &JoinFailed::Registration(words.to_owned()),
            (Path::new("/state"), false),
        )
    };
    let said = member_said(member::NOT_THE_ISSUER);
    assert_eq!(
        member_said(older),
        said,
        "an older member's words read the same way"
    );
    assert!(
        said.contains("this machine dialled 127.0.0.1:49201")
            && said.contains("not its issuer")
            && said.contains("Nothing of the fleet was written here.")
            && said.contains("`aterm fleet peers` on 127.0.0.1:49201")
            && !said.contains("is no fleet's issuer"),
        "the advice names the member it dialled and never quotes its words: {said}"
    );
    let cloned = not_recorded(
        "127.0.0.1:49201",
        &JoinFailed::Registration(format!(
            "{}, and the issuer does not join itself (a Mac cloned or restored from the issuer \
             carries its node)",
            member::OWN_NODE
        )),
        (Path::new("/state"), false),
    );
    assert!(
        cloned.contains(
            "This machine carries the issuer's node (/state/fleet-node, /state/fleet-node-key)"
        ) && cloned.contains("Remove those two files here, and a join mints this machine a node")
            && !cloned.contains("on the issuer (`aterm fleet peers`"),
        "{cloned}"
    );
    let moved = not_recorded(
        "127.0.0.1:49201",
        &JoinFailed::Registration(member::MOVED_ON.to_owned()),
        (Path::new("/state"), false),
    );
    assert!(
        moved.contains("copy the code it issues now")
            && moved.contains(
                "if it left (`aterm fleet leave`), it issues none until `aterm fleet init` runs \
                 there again: copy the code that writes"
            )
            && !moved.contains("put right on the issuer"),
        "{moved}"
    );
    let rotated = not_recorded(
        "192.168.1.23:7100",
        &JoinFailed::Dial(Unreached::Handshake(at, FleetError::ListenerWrongSecret)),
        (Path::new("/state"), false),
    );
    assert!(
        rotated.contains("rotated") && !rotated.contains("start aterm on the issuer"),
        "{rotated}"
    );
    let refused = |at: &str| {
        let to: std::net::SocketAddr = at.parse().unwrap();
        not_recorded(
            at,
            &JoinFailed::Dial(Unreached::Connect(
                to,
                io::Error::from_raw_os_error(libc::ECONNREFUSED),
            )),
            (Path::new("/state"), false),
        )
    };
    assert!(refused("192.168.1.23:7100").contains("local network"));
    let looped = refused("127.0.0.1:7100");
    assert!(
        looped.contains("start aterm on the issuer") && !looped.contains("local network"),
        "a loopback dial asks no consent: {looped}"
    );
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
            ..manual_peer("m9.local".to_owned(), member::default_port())
        });
        Ok(())
    })
    .unwrap();
    let copy = Peer {
        node: Some(n.node().to_owned()),
        key: Some(n.key()),
        via: Via::IssuerBook,
        ..manual_peer("m7.local".to_owned(), member::default_port())
    };
    let by_hand = Peer {
        via: Via::IssuerBook,
        ..manual_peer("m7.local".to_owned(), member::default_port())
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
    assert!(peer_survey(&peer, answered(false), (&[], None, None)).unproven);
    assert!(!peer_survey(&peer, answered(true), (&[], None, None)).unproven);
    let unreached = Unreached::Local("no".to_owned());
    assert!(!peer_survey(&peer, Err(unreached), (&[], None, None)).unproven);
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
fn a_head_of_this_machines_id_another_peer_shares_is_ambiguous_to_every_verb() {
    let world = World::new("own-head");
    ok(init(&world.places, &args(&["--host", "127.0.0.1"])));
    let own = member::node_id(&world.places.state).unwrap();
    let flipped: String = own[8..]
        .chars()
        .map(|c| if c == '0' { '1' } else { '0' })
        .collect();
    let other = format!("{}{flipped}", &own[..8]);
    let port = closed_port();
    member::update_peers(&world.places.state, |book| {
        book.push(Peer {
            node: Some(other.clone()),
            key: Some(NodeIdentity::from_seed(&other, &[5; 32]).unwrap().key()),
            key_from: None,
            host: "127.0.0.1".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port,
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
            signed: None,
        });
        Ok(())
    })
    .unwrap();
    let said = |word: &str| {
        let refused = |result: Result<String, Failure>| match result {
            Err(Failure::Failed(why) | Failure::Usage(why)) => why,
            Ok(report) => panic!("{word}: {report}"),
        };
        let read = survey_fleet_in(
            &world.places,
            &aterm_ctl::FleetAsk {
                host: Some(word),
                within: HANG,
            },
        )
        .expect_err("refused");
        [
            refused(allow(&world.places, &args(&["drive", "--from", word]))),
            refused(peers(&world.places, &args(&["rm", word]))),
            drive_fleet_in(&world.places, word, HANG)
                .err()
                .expect("refused")
                .why,
            read.why,
        ]
    };
    let both = format!(
        "`{}` names 2 nodes ({own}, this machine itself, {other} at 127.0.0.1:{port}): name one \
         by its node id or its address and port",
        &own[..8]
    );
    assert_eq!(said(&own[..8]), [(); 4].map(|()| both.clone()));
    for word in [own.as_str(), &own[..12]] {
        for why in said(word) {
            assert!(
                why.contains("is this machine itself (node "),
                "{word}: {why}"
            );
        }
    }
    assert!(
        read_peers(&world.places.state)
            .unwrap()
            .iter()
            .any(|peer| peer.node.as_deref() == Some(other.as_str())),
        "no word that names both took the other out"
    );
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
        signed: None,
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
    assert!(
        said.contains("names no peer this machine's book records") && !said.contains("issuer's"),
        "{said}"
    );
    let own = member::node_id(&world.places.state).unwrap();
    let listens = fleet_table(&world.config()).unwrap().listen.unwrap();
    let loopback = format!("localhost:{}", member::default_port());
    for word in [
        own.as_str(),
        &own[..8],
        listens.as_str(),
        loopback.as_str(),
        "localhost",
    ] {
        let said = refused(allow(&world.places, &args(&["drive", "--from", word])));
        assert!(said.contains("is this machine itself"), "{word}: {said}");
        let said = refused(peers(&world.places, &args(&["rm", word])));
        assert!(
            said.contains("is this machine itself") && said.contains("`aterm fleet leave`"),
            "{word}: {said}"
        );
        let drives = drive_fleet_in(&world.places, word, HANG)
            .err()
            .expect("its own node");
        assert!(
            drives.why.contains("is this machine itself") && !drives.again,
            "{drives:?}"
        );
        let read = survey_fleet_in(
            &world.places,
            &aterm_ctl::FleetAsk {
                host: Some(word),
                within: HANG,
            },
        )
        .expect_err("its own node");
        assert!(read.why.contains("is this machine itself"), "{read}");
    }
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
    let unanswered = drive_fleet_in(&world.places, &bound, HANG)
        .err()
        .expect("nothing listens there");
    assert!(
        unanswered.why.starts_with("nothing listens on 127.0.0.1:")
            && unanswered.why.matches("127.0.0.1").count() == 1,
        "the peer named once: {unanswered:?}"
    );

    let granted = ok(allow(&world.places, &args(&["drive", "--from=b0b0b0b0"])));
    assert!(
        granted.contains("every member of the fleet now reads this Mac at content, not facts")
            && granted.contains("`aterm fleet allow facts` takes it back, ending every grant")
            && !granted.contains("allow content"),
        "{granted}"
    );
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(table.level(), Some(aterm_net::fleet::DRIVE_LEVEL));
    let granted = format!("{bound}:{}", key.hex());
    assert_eq!(table.drive_from, vec![granted.clone()]);
    assert!(world.config().contains("theme = \"Nord\""));
    let again = ok(allow(&world.places, &args(&["DRIVE", "--from", &bound])));
    assert!(
        again.contains("already") && again.contains("`aterm fleet allow content` ends every grant"),
        "{again}"
    );
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
    assert!(
        removed.contains(&format!(
            "  holds      node {} may drive this Mac no more: ",
            short(&bound)
        )) && removed.contains("its grant")
            && removed.contains("allow is content")
            && removed
                .contains("still reads it at content, which `aterm fleet allow facts` lowers"),
        "{removed}"
    );
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

    let from_content = ok(allow(&world.places, &args(&["drive", "--from", &bound])));
    assert!(
        from_content.contains("reads this Mac at content still")
            && from_content.contains("`aterm fleet allow content` takes it back"),
        "{from_content}"
    );
    ok(allow(&world.places, &args(&["facts"])));
    let from_facts = ok(allow(&world.places, &args(&["drive", "--from", &bound])));
    assert!(
        from_facts.contains("`aterm fleet allow facts` takes it back"),
        "{from_facts}"
    );
    let lowered = ok(allow(&world.places, &args(&["facts"])));
    assert!(
        lowered.contains("its grant of [fleet] drive_from removed"),
        "{lowered}"
    );
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from),
        (Some("facts"), Vec::new())
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
fn a_drive_into_a_place_that_never_handshakes_is_unserved_within_its_deadline() {
    let world = World::new("unserved");
    ok(init(&world.places, &args(&["--host", "127.0.0.1"])));
    // Bound and never accepted: the kernel takes the connection, and no handshake comes.
    let silent = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let node = "e4".repeat(16);
    let key = NodeIdentity::from_seed(&node, &[5; 32]).unwrap().key();
    member::update_peers(&world.places.state, |book| {
        book.push(Peer {
            node: Some(node.clone()),
            key: Some(key),
            key_from: None,
            host: "127.0.0.1".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: silent.local_addr().unwrap().port(),
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
            signed: None,
        });
        Ok(())
    })
    .unwrap();
    // Connected within the hang detector, the handshake it never sends ends the drive there at
    // once ([`member::HeldSilent`]); the bound it would have waited is the rest of the drive's own
    // deadline.
    let (refused, waited) = {
        let held = member::HeldSilent::at(silent.local_addr().unwrap());
        let refused = drive_fleet_in(&world.places, &node, HANG);
        (refused, held.overrode().expect("the drive connected there"))
    };
    assert!(
        !waited.is_zero() && waited <= HANG,
        "the handshake would have waited {waited:?}, past the drive's deadline"
    );
    let refused = refused.err().expect("never served");
    assert!(
        refused.unserved
            && !refused.again
            && refused.why.contains(&format!(
                "no fleet handshake within {:.1}s",
                HANG.as_secs_f64()
            )),
        "{refused:?}"
    );
    let closed = closed_port();
    member::update_peers(&world.places.state, |book| {
        for peer in book.iter_mut() {
            peer.port = closed;
        }
        Ok(())
    })
    .unwrap();
    let refused = drive_fleet_in(&world.places, &node, HANG)
        .err()
        .expect("nothing listens");
    assert!(!refused.unserved, "{refused:?}");
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
        let code = vault_code(&world);
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
    assert!(
        moved.contains("no Mac drives this one")
            && moved.contains("still reads it at content, which `aterm fleet allow facts` lowers"),
        "{moved}"
    );
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from),
        (Some("content"), Vec::new())
    );
}

/// An edit of `[fleet]` whose folder flush fails after its rename is in effect, so it is no
/// failure: the report says where the previous text went and that a power loss may take the
/// edit back, and the verb goes on. A flush that fails before the edit (the `.bak`'s) is the
/// edit's failure, aterm.toml unchanged. With every replace error a failure, as before, the
/// first replace refused.
#[test]
fn an_edit_in_effect_whose_folder_flush_failed_is_said_and_no_failure() {
    use aterm_durable::seam::{EIO, FailingDirs};
    let world = World::new("edit-unflushed");
    world.write_config("font_px = 12.0\n");
    let replaced = member::level_held(&world.places.state, |held| {
        let config = ConfigFile::open(&world.places.config).expect("open");
        let _edit = FailingDirs::after(1, EIO);
        replace_held(held, &config, "font_px = 9.0\n")
    })
    .expect("held")
    .expect("an edit in effect is no failure");
    assert_eq!(world.config(), "font_px = 9.0\n", "the edit is in effect");
    let bak = replaced.bak.as_ref().expect("previous bytes were saved");
    assert_eq!(std::fs::read_to_string(bak).unwrap(), "font_px = 12.0\n");
    assert!(replaced.unflushed.is_some());
    let refused = member::level_held(&world.places.state, |held| {
        let config = ConfigFile::open(&world.places.config).expect("open");
        let _baks = FailingDirs::with(EIO);
        replace_held(held, &config, "font_px = 6.0\n")
    })
    .expect("held");
    assert!(matches!(&refused, Err(Failure::Failed(_))), "{refused:?}");
    assert_eq!(world.config(), "font_px = 9.0\n", "unchanged");
}

#[test]
fn a_level_that_takes_no_drive_away_says_no_drive_was_cut() {
    const CUT: &str = "refuses each new drive";
    let world = World::new("level-uncut");
    ok(init(&world.places, &racing_init()));
    for level in ["facts", "content", "content", "facts"] {
        let said = ok(allow(&world.places, &args(&[level])));
        assert!(!said.contains(CUT), "{level}: {said}");
    }
    let granted = World::new("level-cut");
    granting_issuer(&granted.places);
    let said = ok(allow(&granted.places, &args(&["content"])));
    assert!(said.contains(CUT), "a grant taken out: {said}");
    granted.write_config(&granted.config().replace("\"content\"", "\"drive\""));
    let said = ok(allow(&granted.places, &args(&["facts"])));
    assert!(said.contains(CUT), "drive lowered with no grant: {said}");
}

#[test]
fn every_write_below_drive_takes_the_grants_out_and_says_so() {
    let regrants = "until `aterm fleet allow drive --from <host|node>` grants again";
    let below = |world: &World, report: &str, level: &str| {
        let table = fleet_table(&world.config()).unwrap();
        assert_eq!(
            (table.level(), table.drive_from),
            (Some(level), Vec::new()),
            "{report}"
        );
        assert!(
            report.contains("drive_from removed") && report.contains(regrants),
            "{report}"
        );
    };
    let other = "e7".repeat(16);
    let other_key = NodeIdentity::from_seed(&other, &[6; 32]).unwrap().key();
    let bind_other = |places: &Places| {
        member::update_peers(&places.state, |book| {
            book.push(Peer {
                node: Some(other.clone()),
                key: Some(other_key),
                via: Via::Joined,
                addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
                ..manual_peer("127.0.0.1".to_owned(), closed_port())
            });
            Ok(())
        })
        .unwrap();
    };
    let only_other = vec![format!("{other}:{}", other_key.hex())];
    for words in [
        &["allow", "facts"][..],
        &["allow", "content"],
        &["init", "--rotate"],
        &["init", "--rotate", "--allow", "facts"],
        &["peers", "rm", DRIVER],
    ] {
        let world = World::new("below-issuer");
        let places = &world.places;
        granting_issuer(places);
        bind_other(places);
        let report = ok(match words[0] {
            "allow" => allow(places, &args(&words[1..])),
            "init" => init(places, &args(&words[1..])),
            _ => peers(places, &args(&words[1..])),
        });
        let level = if words.contains(&"facts") {
            "facts"
        } else {
            "content"
        };
        below(&world, &report, level);
        assert_eq!(
            report.contains("which `aterm fleet allow facts` lowers"),
            level == "content",
            "{words:?}: {report}"
        );
        if words[0] == "init" {
            assert!(
                report.contains("as the fleet's secret is new"),
                "{words:?}: {report}"
            );
        }
        ok(allow(places, &args(&["drive", "--from", &other])));
        assert_eq!(
            fleet_table(&world.config()).unwrap().drive_from,
            only_other,
            "{words:?}: a grant after it names its node alone"
        );
    }
    // A list a hand left below drive grants nothing, so a grant starts it afresh.
    let world = World::new("below-by-hand");
    granting_issuer(&world.places);
    bind_other(&world.places);
    world.write_config(&world.config().replace("\"drive\"", "\"content\""));
    assert_eq!(fleet_table(&world.config()).unwrap().drive_from.len(), 1);
    ok(allow(&world.places, &args(&["drive", "--from", &other])));
    assert_eq!(fleet_table(&world.config()).unwrap().drive_from, only_other);
    // A hand's list at facts left the members reading at facts, so content raises them.
    for (was, still) in [("facts", false), ("content", true)] {
        let world = World::new("below-by-hand-level");
        granting_issuer(&world.places);
        world.write_config(&world.config().replace("\"drive\"", &format!("\"{was}\"")));
        let report = ok(allow(&world.places, &args(&["content"])));
        below(&world, &report, "content");
        assert_eq!(
            report.contains("every member of the fleet still reads it at content"),
            still,
            "{was}: {report}"
        );
    }

    let (issuer, code, serving) = issuing("below-issuer-live");
    let member = World::new("below-member");
    let opts = |allow| Opts {
        allow,
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts(None), &code));
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    for level in [FleetScope::Facts, FleetScope::Content] {
        ok(allow(
            &member.places,
            &args(&["drive", "--from", &issuer_node]),
        ));
        let report = ok(join(&member.places, &opts(Some(level)), &code));
        below(&member, &report, level.word());
    }
    // The issuer's rotate takes its member's grants out at its join of the new code too.
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    let port = hinted_port(&code);
    drop(serving);
    ok(init(&issuer.places, &args(&["--rotate"])));
    let rotated = vault_code(&issuer);
    let serving = Listener::serve(
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap(),
        &issuer.places.state,
    );
    let renewed = |report: &str| {
        below(&member, report, "content");
        assert!(
            report.contains("in place of the secret its issuer held before\n")
                && report.contains("as the fleet's secret is new")
                && !report.contains("another fleet"),
            "{report}"
        );
    };
    renewed(&ok(join(&member.places, &opts(None), &rotated)));
    // So do its leave and init again, which keep its node and key: no rotate is named.
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    drop(serving);
    ok(leave(&issuer.places, &[]));
    ok(init(
        &issuer.places,
        &args(&["--host", "127.0.0.1", "--port", &port.to_string()]),
    ));
    let anew = vault_code(&issuer);
    let _serving = Listener::serve(
        TcpListener::bind((Ipv4Addr::LOCALHOST, port)).unwrap(),
        &issuer.places.state,
    );
    let report = ok(join(&member.places, &opts(None), &anew));
    renewed(&report);
    assert!(!report.contains("--rotate"), "{report}");
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
        let code = vault_code(&world);
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
        let code = vault_code(&world);
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
    let _ = aterm_uds::keep_out_of_children(&listener);
    let port = listener.local_addr().unwrap().port().to_string();
    {
        // Served only once init wrote the key it serves: init's proof of its listener there ends
        // at the handshake at once ([`member::HeldSilent`]).
        let _silent = member::HeldSilent::at(listener.local_addr().unwrap());
        ok(init(
            &world.places,
            &args(&["--host", "127.0.0.1", "--port", &port]),
        ));
    }
    let code = placed(&world, ("127.0.0.1", listener.local_addr().unwrap().port()));
    let serving = Listener::serve(listener, &world.places.state);
    (world, code, serving)
}

/// The code `aterm fleet publish` puts in the vault for `world`'s minted code: its secret, then
/// after the `@` the place `(host, port)` a member serves there, which a join dials first.
fn placed(world: &World, (host, port): (&str, u16)) -> String {
    let text = std::fs::read_to_string(world.places.state.join(CODE_FILE)).unwrap();
    JoinCode::parse(&text).unwrap().text_at((host, port))
}

/// [`placed`] at the place `world`'s aterm.toml listens at.
fn vault_code(world: &World) -> String {
    let listen = fleet_table(&world.config()).unwrap().listen.unwrap();
    let (host, port) = member::parse_host_port(&listen, None).unwrap();
    placed(world, (&host, port))
}

/// The port of the place a vault-shaped `code` names after its `@` ([`placed`]).
fn hinted_port(code: &str) -> u16 {
    JoinCode::parse(code)
        .unwrap()
        .hint
        .expect("a placed code")
        .1
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
    ok(with_peers_answering(|| peers(&member.places, &[])));
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
                    let _ = with_peers_answering(|| peers(&elsewhere, &[]));
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
    let rotated = vault_code(&issuer);
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
    let rotated = vault_code(&issuer);
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
        Vec::<String>::new(),
        "a new secret of the same fleet removes them too"
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
        signed: None,
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
    assert_eq!(
        keep_book(&member.places, &secret, (&[], &read), Some(&own)),
        None
    );
    assert!(member::holds_issuer_book(
        &member.places.state,
        &issuer_node
    ));

    ok(leave(&member.places, &[]));
    for read in [read, BookRead::NoIssuer] {
        assert_eq!(
            keep_book(&member.places, &secret, (&[], &read), Some(&own)),
            None
        );
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
    let code = vault_code(&issuer);
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
                signed: None,
            }));
            Ok(())
        })
        .unwrap();
    };
    let grants = || fleet_table(&member.config()).unwrap().drive_from;
    let granted = |key: NodeKey| vec![format!("{node}:{}", key.hex())];
    let read = || check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    // A grant reads the issuer's book for the key it binds now.
    let grant = || {
        ok(with_issuer_serving(|| {
            allow(&member.places, &args(&["drive", "--from", &node]))
        }))
    };
    let whole = aterm_ctl::FleetAsk {
        host: None,
        within: HANG,
    };

    bind(Some(key));
    grant();
    assert_eq!(grants(), granted(key));
    assert_eq!(read().note, None, "a book that still binds it");
    assert_eq!(grants(), granted(key), "keeps the grant");

    // A level a hand left below drive: a read that takes no grant out writes nothing, and one
    // that takes one out takes the rest with it, saying why.
    let issuer_node = member::node_id(&issuer.places.state).unwrap();
    ok(allow(
        &member.places,
        &args(&["drive", "--from", &issuer_node]),
    ));
    let lowered = member.config().replace("\"drive\"", "\"content\"");
    member.write_config(&lowered);
    assert_eq!(read().note, None, "a book that still binds both");
    assert_eq!(member.config(), lowered, "a read writes nothing");
    bind(None);
    let note = read().note.unwrap_or_default();
    assert!(
        note.starts_with(
            "Its issuer's book binds a node this Mac let drive it no more, or to another key, so \
             its grant of [fleet] drive_from was removed from "
        ) && note.ends_with(
            ", with the 1 more it listed, as allow is content, below drive: no Mac drives this \
             one until `aterm fleet allow drive --from <host|node>` grants again, and every \
             member of the fleet still reads it at content, which `aterm fleet allow facts` \
             lowers."
        ) && !note.contains("drive      "),
        "{note}"
    );
    assert_eq!(grants(), Vec::<String>::new());
    bind(Some(key));
    grant();
    assert_eq!(grants(), granted(key));

    bind(None);
    let listed = ok(list_peers(&member.places, HANG));
    assert!(
        listed.contains("its grant of [fleet] drive_from was removed")
            && listed.contains("and allow is content")
            && listed
                .lines()
                .next()
                .is_some_and(|head| head.contains("and allow is content, but")),
        "the header says the level the read's own write left: {listed}"
    );
    assert_eq!(grants(), Vec::<String>::new(), "the issuer took it out");
    bind(Some(key));
    let _ = read();
    assert_eq!(
        grants(),
        Vec::<String>::new(),
        "a join again under the old key is granted nothing"
    );

    grant();
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
    grant();
    assert_eq!(grants(), granted(key), "the key the issuer binds now");

    let copied = || {
        member::read_issuer_book(&member.places.state, &issuer_node)
            .into_iter()
            .filter_map(|peer| peer.node)
            .collect::<Vec<_>>()
    };
    let removed = ok(peers(&member.places, &args(&["rm", &node])));
    assert!(
        removed.contains("drives this Mac no more")
            && removed.ends_with(&format!(
                "  holds      node {} may drive this Mac no more: an aterm running here refuses \
                 its next drive from now on, and cuts its open drives and lifts any hold it took \
                 here at its next read of [fleet] (each 2 s), so a write here before then may \
                 still meet `ERR halted`\n",
                short(&node)
            )),
        "{removed}"
    );
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
    grant();
    ok(allow(&member.places, &args(&["drive", "--all"])));
    let removed = ok(peers(&member.places, &args(&["rm", &node])));
    assert!(
        removed.contains("it may still drive this Mac, as [fleet] drive_from's \"*\"")
            && !removed.contains("  holds  "),
        "{removed}"
    );
    assert_eq!(grants(), vec![aterm_net::fleet::ALL_MEMBERS.to_owned()]);
    // Its issuer taken out takes the copy with it, and every node only the copy bound with that.
    let removed = ok(peers(&member.places, &args(&["rm", &issuer_node])));
    assert!(
        removed.contains(", and this machine's copy of its book")
            && removed.contains(&format!(
                "  holds      nodes {}, {} may drive this Mac no more: an aterm running here \
                 refuses their next drive",
                short(&issuer_node),
                short(&node)
            )),
        "{removed}"
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
    let code = vault_code(&issuer);
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
            host: "lab-mini.invalid".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: third_port,
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
            signed: None,
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
    let words = [third.as_str(), node.as_str(), by_name.as_str()];
    let grant: Vec<&str> = std::iter::once("drive")
        .chain(words.iter().flat_map(|&word| ["--from", word]))
        .collect();
    ok(with_issuer_serving(|| allow(&member.places, &args(&grant))));
    assert_eq!(grants(), granted);
    let targets = known_peers(&member.places).unwrap_or_else(|f| panic!("{f:?}"));
    for word in words {
        match bound_peer(&member.places, word, None) {
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
            dialled
                .why
                .starts_with(&format!("lab-mini.invalid:{third_port}: ")),
            "{word}: {dialled:?}"
        );
    }
    for word in words {
        let ask = aterm_ctl::FleetAsk {
            host: Some(word),
            within: Duration::from_millis(100),
        };
        let read =
            survey_fleet_in(&member.places, &ask).unwrap_or_else(|why| panic!("{word}: {why}"));
        let rows: Vec<(&str, Option<&str>)> = read
            .peers
            .iter()
            .map(|peer| (peer.label.as_str(), peer.node.as_deref()))
            .collect();
        assert_eq!(
            rows,
            [(
                format!("lab-mini.invalid:{third_port}").as_str(),
                Some(node.as_str())
            )],
            "{word}: the node bound at the place, as a drive dials it"
        );
    }

    let two = format!(
        "`lab-mini` names 2 peers ({node} at {third}, - at {elsewhere}): name one by its node id \
         or its address and port"
    );
    let granted_by_name = allow(&member.places, &args(&["drive", "--from", "lab-mini"]));
    assert!(
        matches!(&granted_by_name, Err(Failure::Failed(why)) if *why == two),
        "{granted_by_name:?}"
    );
    let ask = aterm_ctl::FleetAsk {
        host: Some("lab-mini"),
        within: Duration::from_millis(100),
    };
    assert_eq!(
        survey_fleet_in(&member.places, &ask)
            .map(|read| read.peers.len())
            .map_err(|refused| refused.why),
        Err(two.clone())
    );
    let driven = drive_fleet_in(&member.places, "lab-mini", Duration::from_secs(1))
        .err()
        .expect("refused before any dial");
    assert_eq!(
        driven.why, two,
        "a name the node and an entry elsewhere answer to"
    );

    for word in [alone.as_str(), elsewhere.as_str()] {
        assert!(
            matches!(bound_peer(&member.places, word, None), Ok(Bound::ByHand(_))),
            "{word}"
        );
        let refused = drive_fleet_in(&member.places, word, Duration::from_secs(1))
            .err()
            .expect("refused before any dial");
        assert!(refused.why.contains("named by hand"), "{word}: {refused:?}");
    }

    // A second node the issuer's book records at the entry's place: the entry names neither, and
    // the refusal names each by its whole id.
    let second = "e6".repeat(16);
    let second_key = NodeIdentity::from_seed(&second, &[9; 32]).unwrap().key();
    member::update_peers(&issuer.places.state, |book| {
        book.push(Peer {
            node: Some(second.clone()),
            key: Some(second_key),
            key_from: None,
            host: "lab-mini.invalid".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: third_port,
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
            signed: None,
        });
        Ok(())
    })
    .unwrap();
    check_peers(&member.places, HANG).unwrap_or_else(|f| panic!("{f:?}"));
    let refused = bound_peer(&member.places, &by_name, None);
    assert!(
        matches!(&refused, Err(Failure::Failed(why)) if why.contains(&format!("at the place of 2 nodes ({node}, {second})"))),
        "{refused:?}"
    );
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
    off: bool,
    synced: &'static str,
    owed: bool,
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
    // A member a sync put in B's fleet, with B's default grant.
    SyncedB,
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
    // A level's write (`allow content`), tried inside a step under the book's lock too.
    Level,
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
        verb: "join again at facts",
        fixture: Fixture::Granted,
        steps: &["lock", "key", "drop", "config", "book"],
        run: |racing, places, _| {
            let opts = Opts {
                allow: Some(FleetScope::Facts),
                ..racing_opts()
            };
            join(places, &opts, &racing.a.1)
        },
        again: None,
        alone: |end| end.level == Some("facts") && end.grants.is_empty(),
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
        run: |_, places, _| with_peers_answering(|| peers(places, &[])),
        again: None,
        alone: |end| end.copy == "A",
    },
    Race {
        verb: "leave",
        fixture: Fixture::Granted,
        steps: &["removed", "config", "unlisten"],
        run: |_, places, _| leave(places, &[]),
        again: None,
        alone: |end| end.key == "-" && !end.listen,
    },
    Race {
        verb: "leave of no state directory",
        fixture: Fixture::Stale,
        steps: &["removed", "config", "unlisten"],
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
        alone: |end| end.key == "minted" && end.level == Some("content") && end.grants.is_empty(),
    },
    Race {
        verb: "rotate at facts",
        fixture: Fixture::Issuer,
        steps: &["key", "code", "config"],
        run: |_, places, _| init(places, &args(&["--rotate", "--allow", "facts"])),
        again: None,
        alone: |end| end.key == "minted" && end.level == Some("facts") && end.grants.is_empty(),
    },
    Race {
        verb: "sync's first join",
        fixture: Fixture::Empty,
        steps: &["lock", "key", "drop", "config", "grant", "book"],
        run: |racing, places, _| syncing(places, false, &racing.a.1),
        again: None,
        alone: |end| end.key == "A" && end.grants == ["all"] && end.synced == "A" && !end.owed,
    },
    Race {
        verb: "sync that follows the vault's new code",
        fixture: Fixture::SyncedB,
        steps: &["lock", "key", "drop", "config", "grant", "book"],
        run: |racing, places, _| syncing(places, false, &racing.a.1),
        again: None,
        alone: |end| end.key == "A" && end.grants == ["all"] && end.synced == "A" && !end.owed,
    },
    Race {
        verb: "sync --on after a stopped leave",
        fixture: Fixture::Left,
        steps: &["on", "lock", "key", "drop", "config", "book"],
        run: |racing, places, _| syncing(places, true, &racing.a.1),
        again: None,
        alone: |end| {
            end.key == "A" && end.level == Some("content") && !end.off && end.synced == "A"
        },
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

// A sync of the vault's default fleet `code`, a person's `--on` where `on`: a fleet it leaves as it
// is, as `aterm fleet sync` says, did not do what it was asked.
fn syncing(places: &Places, on: bool, code: &str) -> Result<String, Failure> {
    match sync_with(places, on, publishes(code), &racing_opts())? {
        FleetSync::Blocked(why) => Err(failed(why)),
        synced => Ok(synced.report()),
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
            signed: None,
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
        host: "lab-mini.invalid".to_owned(),
        port: 7199,
        version: "0.130.0".to_owned(),
        key: None,
        signed: None,
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
        Other::Level => drop(allow(places, &args(&["content"]))),
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
                ok(with_peers_answering(|| peers(places, &[])));
                if matches!(fixture, Fixture::Left) {
                    interfere(Other::StoppedLeave, &self.c.1, places);
                }
            }
            Fixture::Issuer => granting_issuer(places),
            Fixture::Unlistened => {
                assert!(killed_at("config", || init(places, &racing_init())));
            }
            Fixture::SyncedB => {
                ok(syncing(places, false, &self.b.1));
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
                .map(|word| {
                    if word == aterm_net::fleet::ALL_MEMBERS {
                        "all"
                    } else {
                        node_role(&DriveGrant::node_of(word))
                    }
                })
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
            off: state.join(member::OFF_FILE).exists(),
            synced: synced(&world.places).map_or("-", |mark| {
                fleets
                    .iter()
                    .find(|(_, key, _)| key.mark() == mark)
                    .map_or("?", |f| f.0)
            }),
            owed: state.join(GRANT_OWED_FILE).exists(),
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
                                    *busy.borrow_mut() = Some(
                                        member::lock_waiting(HANG, || member::hold_book(dir))
                                            .unwrap(),
                                    )
                                }
                                other => interfere(other, &c_code, &here),
                            }
                        } else if pending.get() && matches!(other, Other::Level) {
                            // Inside the step: only a level written without the book's lock lands here.
                            let landed = member::lock_waiting(Duration::ZERO, || {
                                allow(&here, &args(&["content"]))
                            });
                            pending.set(landed.is_err());
                        }
                    }
                };
                let run = || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        with_step_hook(hook, verb)
                    }))
                };
                // The hold lasts until the verb returns, so a wait of the verb's for the book
                // past it ends refused however long it is: it waits none. The panic is caught
                // inside the seam, so the thread's wait comes back as it was.
                let ran = if matches!(other, Other::Busy) {
                    member::lock_waiting(Duration::ZERO, run)
                } else {
                    run()
                };
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
    let mut ends = vec![alone.clone()];
    for other in [
        Other::Leave,
        Other::StoppedLeave,
        Other::JoinC,
        Other::Init,
        Other::Kill,
        Other::Busy,
        Other::Level,
    ] {
        let (mut serial, instead) = if matches!(other, Other::Kill | Other::Busy) {
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
        // A leave that meets no `[fleet]` in aterm.toml (one the verb has not written yet) is a
        // whole leave, whatever holds aterm.toml's lock.
        if matches!(other, Other::StoppedLeave) {
            serial.push(racing.run(race, Some((Other::Leave, When::After))).0);
        }
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
            ends.push(end);
        }
        ends.extend(serial.into_iter().chain(instead));
    }
    // A level below drive keeps no grant, whatever comes between.
    unserial.extend(
        ends.iter()
            .filter(|end| {
                end.level != Some(aterm_net::fleet::DRIVE_LEVEL) && !end.grants.is_empty()
            })
            .map(|end| format!("{verb} ends granting below drive:\n{end:#?}")),
    );
    unserial
}

#[test]
fn every_fleet_writer_ends_as_some_serial_order_would() {
    // The races are independent and mostly file system work, so they run on as many workers as
    // the kernel serves file system calls fastest for (`aterm_spec::fs_lane::PLACES`), each making
    // its issuers. A race's panic is one of its lines, so every race runs and reports, as when
    // each had a thread of its own.
    let races: Vec<_> = RACES.iter().enumerate().collect();
    let unserial: Vec<String> =
        aterm_spec::check::each_on(aterm_spec::fs_lane::PLACES, &races, |&(at, one)| {
            aterm_spec::check::attempt(|| {
                let [a, b, c] = ["a", "b", "c"].map(|name| issuing(&format!("race{at}-{name}")));
                race(&Racing { a, b, c }, one)
            })
            .unwrap_or_else(|why| vec![format!("race {at} ({}) panicked: {why}", one.verb)])
        })
        .concat();
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
    assert!(
        inited.contains("as they named the nodes of the fleet this machine held before"),
        "{inited}"
    );
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
    assert_eq!(fleet_table(&world.config()).unwrap().drive_from.len(), 0);
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
    let named = |cause: &str, act: &str| {
        format!("{cause} ({}), so nothing was granted: {act}", key.display())
    };
    assert!(
        matches!(&unread, Err(Failure::Failed(why)) if *why == named("fleet.key is open to other users", "`chmod 600` it, then run this again")),
        "{unread:?}"
    );
    assert_eq!(world.config(), config, "nothing granted");
    let again = init(places, &[]);
    assert!(
        matches!(&again, Err(Failure::Failed(why)) if why.starts_with("fleet.key is open to other users (") && why.ends_with("so this init cannot tell which fleet this machine issues, and wrote nothing: `chmod 600` it, then run this again; `aterm fleet leave` leaves the fleet")),
        "{again:?}"
    );
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let link = places.config.with_file_name("fleet.key.copy");
    std::fs::hard_link(&key, &link).unwrap();
    let linked = allow(places, &args(&["drive", "--from", DRIVER]));
    assert!(
        matches!(&linked, Err(Failure::Failed(why)) if *why == named("fleet.key has another hard link, a second copy of the fleet's secret", "remove that link, run this again")),
        "{linked:?}"
    );
    assert_eq!(world.config(), config, "nothing granted");
    std::fs::remove_file(&link).unwrap();
    // An issuer's key that holds no secret: its fleet is made again, which no join reaches.
    let secret = std::fs::read(&key).unwrap();
    write_private(&places.state, KEY_FILE, b"not a secret\n").unwrap();
    let malformed = allow(places, &args(&["drive", "--from", DRIVER]));
    assert!(
        matches!(&malformed, Err(Failure::Failed(why)) if why.starts_with("fleet.key holds no fleet secret: ") && why.ends_with("so nothing was granted: `aterm fleet leave`, `init` again")),
        "{malformed:?}"
    );
    let again = init(places, &[]);
    assert!(
        matches!(&again, Err(Failure::Failed(why)) if why.ends_with("and wrote nothing: `aterm fleet leave`, `init` again") && !why.contains("join again")),
        "{again:?}"
    );
    assert_eq!(world.config(), config, "nothing granted");
    write_private(&places.state, KEY_FILE, &secret).unwrap();
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

fn granting_member(tag: &str) -> (World, Vec<String>, (World, String, Listener)) {
    let issuer = issuing(&format!("{tag}-issuer"));
    let member = World::new(&format!("{tag}-member"));
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &issuer.1));
    let node = member::node_id(&issuer.0.places.state).unwrap();
    (member, args(&["drive", "--from", &node]), issuer)
}

#[test]
fn a_leave_that_cannot_write_fleet_off_removes_nothing() {
    let (member, _grant, _issuer) = granting_member("off-refused");
    ok(allow(&member.places, &args(&["drive", "--all"])));
    let off = member.places.state.join(member::OFF_FILE);
    let (config, key) = (
        member.config(),
        read_key_file(&member.places.state).unwrap(),
    );
    let refused = member::off_refused(std::io::ErrorKind::StorageFull, || {
        leave(&member.places, &[])
    });
    assert!(
        matches!(&refused, Err(Failure::Failed(why)) if why.starts_with(&format!("{} could not be written (", off.display())) && why.contains("so nothing was removed") && why.ends_with("run `aterm fleet leave` again")),
        "{refused:?}"
    );
    assert_eq!(
        (
            member.config(),
            read_key_file(&member.places.state).unwrap()
        ),
        (config, key),
        "nothing removed"
    );
    assert!(!off.exists());
    ok(leave(&member.places, &[]));
    assert!(off.is_file() && read_key_file(&member.places.state).is_err());
}

#[test]
fn all_members_can_drive_and_an_explicit_leave_stays_off_until_a_rejoin() {
    let (member, _grant, issuer) = granting_member("grant-all");
    ok(allow(&member.places, &args(&["drive", "--all"])));
    let table = fleet_table(&member.config()).expect("fleet table");
    assert_eq!(table.allow.as_deref(), Some("drive"));
    assert_eq!(table.drive_from, [aterm_net::fleet::ALL_MEMBERS]);
    let all = ok(allow(&member.places, &args(&["drive", "--all"])));
    assert!(
        all.contains("every member the fleet's issuer binds") && !all.contains("each node named"),
        "{all}"
    );
    let lowered = ok(allow(&member.places, &args(&["content"])));
    assert!(
        lowered.contains(
            "until `aterm fleet allow drive --all` (every member, as before) or `--from \
             <host|node>` grants again"
        ),
        "{lowered}"
    );
    ok(allow(&member.places, &args(&["drive", "--all"])));
    let off = member.places.state.join(member::OFF_FILE);
    ok(leave(&member.places, &[]));
    assert!(off.is_file());
    let again = ok(leave(&member.places, &[]));
    assert!(off.is_file() && !again.contains("  off "), "{again}");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    let joined = ok(join(&member.places, &opts, &issuer.1));
    assert!(
        !off.exists() && joined.contains("fleet sync is on again"),
        "{joined}"
    );
    ok(leave(&member.places, &[]));
    let synced = Opts {
        synced: Some(Held::Nothing),
        port: opts.port,
        ..Opts::default()
    };
    std::fs::remove_file(&off).unwrap();
    let leaving = off.clone();
    let raced = with_step_hook(
        move |at| {
            if at == "lock" {
                std::fs::write(&leaving, b"").unwrap();
            }
        },
        || join(&member.places, &synced, &issuer.1),
    );
    assert!(
        matches!(&raced, Err(Failure::Failed(why)) if why.starts_with("fleet sync is off after aterm fleet leave, which came while it joined")),
        "{raced:?}"
    );
    assert!(off.is_file() && read_key_file(&member.places.state).is_err());
    std::fs::remove_file(&off).unwrap();
    std::fs::create_dir_all(off.join("kept")).unwrap();
    let kept = ok(join(&member.places, &opts, &issuer.1));
    assert!(
        kept.contains("could not be removed") && kept.contains("so fleet sync stays off"),
        "a join that succeeded says so: {kept}"
    );
}

#[test]
fn a_grant_a_leave_comes_into_refuses_as_one_after_the_leave_would() {
    let (member, grant, _issuer) = granting_member("grant-left");
    let places = Places {
        state: member.places.state.clone(),
        config: member.places.config.clone(),
    };
    let left = Rc::new(RefCell::new(None));
    let hook = {
        let left = left.clone();
        move |step: &str| {
            if step == "grant" && left.borrow().is_none() {
                *left.borrow_mut() = Some(leave(&places, &[]).map_err(failure_words));
            }
        }
    };
    let granted = with_step_hook(hook, || allow(&member.places, &grant)).map_err(failure_words);
    assert!(matches!(left.borrow_mut().take(), Some(Ok(_))));
    let serial = allow(&member.places, &grant).map_err(failure_words);
    assert!(
        matches!(&serial, Err(why) if why.starts_with("this machine is in no fleet")),
        "{serial:?}"
    );
    assert_eq!(granted, serial);
}

#[test]
fn a_grant_that_reads_a_leave_half_done_refuses_as_one_after_it_would() {
    let (member, grant, _issuer) = granting_member("grant-half");
    let places = Places {
        state: member.places.state.clone(),
        config: member.places.config.clone(),
    };
    let granting = Rc::new(RefCell::new(None));
    let hook = {
        let (granting, wanted) = (granting.clone(), grant.clone());
        let (told, waits) = std::sync::mpsc::channel();
        move |step: &str| {
            if step != "removed" || granting.borrow().is_some() {
                return;
            }
            let (state, config, grant, told) = (
                places.state.clone(),
                places.config.clone(),
                wanted.clone(),
                told.clone(),
            );
            *granting.borrow_mut() = Some(std::thread::spawn(move || {
                let places = Places { state, config };
                let granted =
                    member::lock_waiting_told(HANG, told.clone(), || allow(&places, &grant));
                let _ = told.send(());
                granted.map_err(failure_words)
            }));
            // The leave holds the book's lock, past the fleet's files and before aterm.toml.
            waits
                .recv_timeout(HANG)
                .expect("the grant waits for the lock or ends");
        }
    };
    let left = with_step_hook(hook, || leave(&member.places, &[]));
    assert!(left.is_ok(), "{left:?}");
    let granted = granting
        .borrow_mut()
        .take()
        .expect("the grant ran beside the leave")
        .join()
        .unwrap();
    let serial = allow(&member.places, &grant).map_err(failure_words);
    assert!(
        matches!(&serial, Err(why) if why.starts_with("this machine is in no fleet")),
        "{serial:?}"
    );
    assert_eq!(granted, serial);
}

fn copied_member(issuer: &World) -> (String, NodeKey) {
    let node = "e7".repeat(16);
    let key = NodeIdentity::from_seed(&node, &[9; 32]).unwrap().key();
    member::update_peers(&issuer.places.state, |book| {
        book.push(Peer {
            node: Some(node.clone()),
            key: Some(key),
            key_from: None,
            host: "127.0.0.1".to_owned(),
            addr: Some(IpAddr::from(Ipv4Addr::LOCALHOST)),
            port: closed_port(),
            version: None,
            seen_ms: None,
            via: Via::Joined,
            fleet: None,
            signed: None,
        });
        Ok(())
    })
    .unwrap();
    (node, key)
}

#[test]
fn a_grant_that_reads_a_first_join_half_done_reads_again_and_grants_as_after_it() {
    let (issuer, code, _serving) = issuing("grant-join-issuer");
    let (node, key) = copied_member(&issuer);
    let member = World::new("grant-join-member");
    let grant = args(&["drive", "--from", &node]);
    let granting = Rc::new(RefCell::new(None));
    let hook = {
        let (granting, grant) = (granting.clone(), grant.clone());
        let (state, config) = (member.places.state.clone(), member.places.config.clone());
        move |step: &str| {
            if step != "config" || granting.borrow().is_some() {
                return;
            }
            let places = Places {
                state: state.clone(),
                config: config.clone(),
            };
            let grant = grant.clone();
            let (read, reads) = std::sync::mpsc::channel();
            let (joined, joins) = std::sync::mpsc::channel::<()>();
            let granted = std::thread::spawn(move || {
                let mut first = true;
                let hook = move |step: &str| {
                    if step == "grant" && std::mem::take(&mut first) {
                        read.send(()).unwrap();
                        joins.recv_timeout(HANG).expect("the join ends");
                    }
                };
                with_issuer_serving(|| with_step_hook(hook, || allow(&places, &grant)))
                    .map_err(failure_words)
            });
            // The key is written and aterm.toml not yet: the grant reads there, and takes the lock
            // once the join is whole.
            reads.recv_timeout(HANG).expect("the grant reads");
            *granting.borrow_mut() = Some((granted, joined));
        }
    };
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(with_step_hook(hook, || join(&member.places, &opts, &code)));
    let (granted, joined) = granting
        .borrow_mut()
        .take()
        .expect("the grant ran beside the join");
    joined.send(()).unwrap();
    let granted = granted.join().unwrap();
    assert!(granted.is_ok(), "{granted:?}");
    assert_eq!(
        fleet_table(&member.config()).unwrap().drive_from,
        [format!("{node}:{}", key.hex())]
    );
}

#[test]
fn a_grant_whose_fleet_moves_at_each_read_refuses_saying_so() {
    let (issuer, code, _serving) = issuing("grant-moving-issuer");
    let (node, _) = copied_member(&issuer);
    let member = World::new("grant-moving-member");
    let opts = Opts {
        port: Some(closed_port()),
        ..Opts::default()
    };
    ok(join(&member.places, &opts, &code));
    let before = member.config();
    let reads = Rc::new(Cell::new(0));
    let hook = {
        let (reads, state) = (reads.clone(), member.places.state.clone());
        move |step: &str| {
            if step != "grant" {
                return;
            }
            reads.set(reads.get() + 1);
            member::update_peers(&state, |book| {
                for peer in book.iter_mut().filter(|peer| peer.via == Via::Issuer) {
                    peer.port = closed_port();
                }
                Ok(())
            })
            .unwrap();
        }
    };
    let granted = with_step_hook(hook, || {
        allow(&member.places, &args(&["drive", "--from", &node]))
    })
    .map_err(failure_words);
    assert_eq!(
        granted,
        Err(format!(
            "the fleet this machine holds moved {GRANT_READS} times while this grant read its \
             issuer's book (an `aterm fleet join`, `leave` or `init` came between), so nothing \
             was granted: run this again"
        ))
    );
    assert_eq!(reads.get(), GRANT_READS);
    assert_eq!(member.config(), before, "nothing granted");
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
    let again = ok(init(places, &asked));
    assert!(again.contains("already the fleet's issuer"), "{again}");
    let table = fleet_table(&world.config()).unwrap();
    assert_eq!(
        (table.listen_port(), table.level()),
        (Some(port), Some(FleetScope::Content.word()))
    );
    let config_after = world.config();
    let whole = ok(init(places, &asked));
    assert!(
        whole.starts_with("aterm fleet init: this machine is already the fleet's issuer;")
            && !whole.contains("written"),
        "the first init's own flags again change nothing: {whole}"
    );
    assert_eq!(world.config(), config_after, "nothing written");
    let moved = init(places, &args(&["--port", &(port ^ 1).to_string()]));
    assert!(
        matches!(&moved, Err(Failure::Usage(why)) if why.contains("another port is aterm.toml's [fleet] listen to edit")),
        "{moved:?}"
    );
    let elsewhere = init(places, &args(&["--host", "m8.local"]));
    assert!(
        matches!(&elsewhere, Err(Failure::Usage(why)) if why.contains("would listen at another place")),
        "{elsewhere:?}"
    );
    let level = init(places, &args(&["--allow", "facts"]));
    assert!(
        matches!(&level, Err(Failure::Usage(why)) if why.ends_with("already the fleet's issuer: a level is set by `aterm fleet allow facts`, not `init`")),
        "{level:?}"
    );
    assert_eq!(world.config(), config_after, "nothing written");
}

/// Where a sync's join listens in a loopback fleet: a port of its own.
fn sync_here() -> Opts {
    Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    }
}

/// A join by hand in a loopback fleet, at the level the join keeps (facts where none is set).
fn by_hand() -> Opts {
    sync_here()
}

/// The vault's answer when it publishes `code` to a non-admin member.
fn publishes(code: &str) -> impl Fn() -> Result<VaultCode, Failure> + '_ {
    publishes_for(code)
}

/// The vault's answer when it publishes `code`, `admin` naming whether this Mac holds its admin
/// locker.
fn publishes_for(code: &str) -> impl Fn() -> Result<VaultCode, Failure> + '_ {
    move || {
        Ok(VaultCode::Code {
            code: aterm_secrets::SecretBytes::new(code.as_bytes().to_vec()),
        })
    }
}

fn converged(result: Result<FleetSync, Failure>) -> FleetSync {
    match result {
        Ok(synced) => synced,
        Err(failure) => panic!("{failure:?}"),
    }
}

#[test]
fn a_sync_grants_drive_on_its_own_join_and_keeps_a_level_set_after() {
    let (issuer, code, _serving) = issuing("sync-grant");
    let member = World::new("sync-grant-member");
    let opts = sync_here();
    let joined = converged(converge_on(&member.places, &code, &opts));
    assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(table.level(), Some(aterm_net::fleet::DRIVE_LEVEL));
    assert_eq!(table.drive_from, [aterm_net::fleet::ALL_MEMBERS]);
    let key = read_key_file(&member.places.state).unwrap();
    assert_eq!(key, read_key_file(&issuer.places.state).unwrap());
    assert_eq!(
        synced(&member.places),
        Some(key.mark()),
        "the join is recorded"
    );
    assert_eq!(
        converged(converge_on(&member.places, &code, &opts)),
        FleetSync::Current
    );
    ok(allow(&member.places, &args(&["content"])));
    let config = member.config();
    assert_eq!(
        converged(converge_on(&member.places, &code, &opts)),
        FleetSync::Current
    );
    assert_eq!(member.config(), config, "the person's level stands");
    let table = fleet_table(&config).unwrap();
    assert_eq!(table.level(), Some(FleetScope::Content.word()));
    assert!(table.drive_from.is_empty(), "{table:?}");
}

#[test]
fn a_sync_leaves_a_fleet_joined_by_hand_and_one_this_mac_issues() {
    let (vaults, code, _serving) = issuing("sync-vaults");
    let (other, other_code, _other_serving) = issuing("sync-other");
    let member = World::new("sync-by-hand");
    let by_hand = || Opts {
        port: Some(closed_port()),
        host: Some("127.0.0.1".to_owned()),
        ..Opts::default()
    };
    ok(join(&member.places, &by_hand(), &other_code));
    let key = read_key_file(&member.places.state).unwrap();
    let config = member.config();
    let blocked = converged(converge_on(&member.places, &code, &sync_here()));
    assert!(
        matches!(&blocked, FleetSync::Blocked(why) if why.contains("joined another fleet by hand")),
        "{blocked:?}"
    );
    assert_eq!(read_key_file(&member.places.state).unwrap(), key);
    assert_eq!(member.config(), config, "nothing written");
    assert_eq!(synced(&member.places), None, "nothing recorded");
    assert!(
        read_peers(&vaults.places.state).unwrap().is_empty(),
        "the vault fleet's issuer was never dialled"
    );
    let issued = read_key_file(&other.places.state).unwrap();
    let blocked = converged(converge_on(&other.places, &code, &sync_here()));
    assert!(
        matches!(&blocked, FleetSync::Blocked(why) if why.contains("issues another fleet")),
        "{blocked:?}"
    );
    assert_eq!(read_key_file(&other.places.state).unwrap(), issued);
    // Joined by hand to the vault's fleet itself, a Mac is in it: recorded, and nothing granted.
    let vaults_by_hand = World::new("sync-by-hand-vaults");
    ok(join(&vaults_by_hand.places, &by_hand(), &code));
    let config = vaults_by_hand.config();
    assert_eq!(
        converged(converge_on(&vaults_by_hand.places, &code, &sync_here())),
        FleetSync::Current
    );
    assert_eq!(vaults_by_hand.config(), config, "nothing granted");
    assert_eq!(
        synced(&vaults_by_hand.places),
        Some(read_key_file(&vaults.places.state).unwrap().mark())
    );
}

#[test]
fn a_sync_follows_the_vaults_new_code_away_from_the_fleet_it_joined() {
    let (_first, first_code, _first_serving) = issuing("sync-first");
    let (second, second_code, _second_serving) = issuing("sync-second");
    let member = World::new("sync-follows");
    let narrowed = World::new("sync-follows-narrowed");
    for world in [&member, &narrowed] {
        let joined = converged(converge_on(&world.places, &first_code, &sync_here()));
        assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    }
    ok(allow(&narrowed.places, &args(&["content"])));
    let second_key = read_key_file(&second.places.state).unwrap();
    for world in [&member, &narrowed] {
        let followed = converged(converge_on(&world.places, &second_code, &sync_here()));
        assert!(matches!(followed, FleetSync::Joined(_)), "{followed:?}");
        assert_eq!(read_key_file(&world.places.state).unwrap(), second_key);
        assert_eq!(synced(&world.places), Some(second_key.mark()));
    }
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(table.level(), Some(aterm_net::fleet::DRIVE_LEVEL));
    assert_eq!(
        table.drive_from,
        [aterm_net::fleet::ALL_MEMBERS],
        "the default grant goes with the fleet"
    );
    let table = fleet_table(&narrowed.config()).unwrap();
    assert_eq!(
        table.level(),
        Some(FleetScope::Content.word()),
        "a narrower level stays narrower"
    );
    assert!(table.drive_from.is_empty(), "{table:?}");
    assert_eq!(read_peers(&second.places.state).unwrap().len(), 2);
}

#[test]
fn a_sync_that_follows_the_vaults_new_code_keeps_a_level_narrowed_to_facts() {
    let (_first, first_code, _first_serving) = issuing("sync-facts-first");
    let (second, second_code, _second_serving) = issuing("sync-facts-second");
    let synced_here = World::new("sync-facts-synced");
    let joined = converged(converge_on(&synced_here.places, &first_code, &sync_here()));
    assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    ok(allow(&synced_here.places, &args(&["facts"])));
    // Joined by hand to the code the vault published then, at the join's own level (facts): in
    // the vault's fleet, which a sync follows with it.
    let by_hand_vaults = World::new("sync-facts-by-hand");
    ok(join(&by_hand_vaults.places, &by_hand(), &first_code));
    assert_eq!(
        converged(converge_on(
            &by_hand_vaults.places,
            &first_code,
            &sync_here()
        )),
        FleetSync::Current
    );
    let second_key = read_key_file(&second.places.state).unwrap();
    for world in [&synced_here, &by_hand_vaults] {
        let followed = converged(converge_on(&world.places, &second_code, &sync_here()));
        assert!(matches!(followed, FleetSync::Joined(_)), "{followed:?}");
        assert_eq!(read_key_file(&world.places.state).unwrap(), second_key);
        let table = fleet_table(&world.config()).unwrap();
        assert_eq!(
            table.level(),
            Some(FleetScope::Facts.word()),
            "a level narrowed to facts is never widened by a sync's join"
        );
        assert!(table.drive_from.is_empty(), "{table:?}");
    }
}

#[test]
fn the_way_out_a_blocked_sync_names_moves_the_mac_to_the_default_fleet() {
    let (vaults, code, _serving) = issuing("sync-way-out-vaults");
    let (other, other_code, _other_serving) = issuing("sync-way-out-other");
    let member = World::new("sync-way-out-member");
    ok(join(&member.places, &by_hand(), &other_code));
    let vault = publishes(&code);
    for world in [&member, &other] {
        let blocked = converged(sync_with(&world.places, false, &vault, &sync_here()));
        assert!(
            matches!(&blocked, FleetSync::Blocked(why) if why.contains("`aterm fleet sync --on`")),
            "{blocked:?}"
        );
        ok(leave(&world.places, &[]));
        assert_eq!(
            converged(sync_with(&world.places, false, &vault, &sync_here())),
            FleetSync::Off,
            "a leave keeps the subscriber's sync off"
        );
        let joined = converged(sync_with(&world.places, true, &vault, &sync_here()));
        assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
        assert!(!in_state(&world.places, member::OFF_FILE).unwrap());
        let key = read_key_file(&world.places.state).unwrap();
        assert_eq!(key, read_key_file(&vaults.places.state).unwrap());
        assert_eq!(synced(&world.places), Some(key.mark()));
        let table = fleet_table(&world.config()).unwrap();
        assert_eq!(table.level(), Some(aterm_net::fleet::DRIVE_LEVEL));
        assert_eq!(table.drive_from, [aterm_net::fleet::ALL_MEMBERS]);
    }
}

/// Every file of `world`'s fleet state and its aterm.toml, with its bytes: what a read leaves as
/// it found it.
fn files_of(world: &World) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_default();
                out.push((path, bytes));
            }
        }
    }
    let mut out = Vec::new();
    walk(&world.places.state, &mut out);
    let config = std::fs::read(&world.places.config).unwrap_or_default();
    out.push((world.places.config.clone(), config));
    out.sort();
    out
}

fn checked(result: Result<FleetCheck, Failure>) -> FleetCheck {
    match result {
        Ok(check) => check,
        Err(failure) => panic!("{failure:?}"),
    }
}

/// The converge pass's read agrees with the sync it precedes, and writes nothing: a Mac in no
/// fleet, or in another fleet a person joined, reads due (the sync then joins, or leaves it as
/// it is); one a person joined to the vault's own fleet reads due until a sync records it as the
/// fleet it follows; once synced, current; after a leave, idle.
/// NEGATIVE CONTROL: a check that skips the `fleet-synced` record reads the Mac joined by hand
/// current, while the sync it precedes would still write that record.
#[test]
fn the_fleet_check_reads_what_a_sync_would_do_and_writes_nothing() {
    let (_vaults, code, _serving) = issuing("check-vaults");
    let (_other, other_code, _other_serving) = issuing("check-other");
    let due = |check: FleetCheck, says: &str| {
        assert!(
            matches!(&check, FleetCheck::Due(why) if why.contains(says)),
            "{says}: {check:?}"
        );
    };
    let member = World::new("check-member");
    let before = files_of(&member);
    due(checked(check_on(&member.places, &code)), "in no fleet");
    assert_eq!(files_of(&member), before, "the read wrote nothing");
    let joined = converged(converge_on(&member.places, &code, &sync_here()));
    assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    let synced_files = files_of(&member);
    assert_eq!(
        checked(check_on(&member.places, &code)),
        FleetCheck::Current
    );
    assert_eq!(files_of(&member), synced_files, "the read wrote nothing");

    let vaults_by_hand = World::new("check-by-hand-vaults");
    ok(join(&vaults_by_hand.places, &by_hand(), &code));
    due(
        checked(check_on(&vaults_by_hand.places, &code)),
        "not yet recorded",
    );
    assert_eq!(
        converged(converge_on(&vaults_by_hand.places, &code, &sync_here())),
        FleetSync::Current
    );
    assert_eq!(
        checked(check_on(&vaults_by_hand.places, &code)),
        FleetCheck::Current
    );

    let other_by_hand = World::new("check-by-hand-other");
    ok(join(&other_by_hand.places, &by_hand(), &other_code));
    due(
        checked(check_on(&other_by_hand.places, &code)),
        "another fleet's key",
    );
    let blocked = converged(converge_on(&other_by_hand.places, &code, &sync_here()));
    assert!(matches!(blocked, FleetSync::Blocked(_)), "{blocked:?}");

    ok(leave(&member.places, &[]));
    assert_eq!(
        checked(check_with(&member.places, publishes(&code))),
        FleetCheck::Idle(FleetSync::Off)
    );
}

/// The converge pass's read finds every state the sync it precedes still acts on, and writes
/// nothing: a sync's join cut short at its grant (its join's own aterm.toml take), a default
/// grant an older aterm's pass left owed beside the join it finished, a join that stopped before
/// its book named the issuer, a Mac that issues another fleet, and a key that cannot be read each
/// read due; the cut-short join and the owed grant read current once a sync finishes them.
/// NEGATIVE CONTROL: a check that skips the owed-grant record reads that Mac current, so the
/// pass would never write the grant.
#[test]
fn the_fleet_check_reads_an_owed_grant_an_unfinished_join_an_issuer_and_an_unread_key_as_due() {
    let (_vaults, code, _serving) = issuing("check-owed-vaults");
    let (other, _other_code, _other_serving) = issuing("check-owed-other");
    let due = |world: &World, says: &str| {
        let before = files_of(world);
        let check = checked(check_on(&world.places, &code));
        assert!(
            matches!(&check, FleetCheck::Due(why) if why.contains(says)),
            "{says}: {check:?}"
        );
        assert_eq!(files_of(world), before, "{says}: the read wrote nothing");
    };

    let owed = World::new("check-owed-member");
    assert!(
        killed_at("grant", || {
            converge_on(&owed.places, &code, &sync_here()).map(|synced| synced.report())
        }),
        "the pass is cut short at its grant"
    );
    assert!(in_state(&owed.places, GRANT_OWED_FILE).unwrap());
    due(&owed, "did not finish");
    let joined = converged(converge_on(&owed.places, &code, &sync_here()));
    assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    assert_eq!(checked(check_on(&owed.places, &code)), FleetCheck::Current);
    ok(allow(&owed.places, &args(&["content"])));
    write_private(&owed.places.state, GRANT_OWED_FILE, b"").unwrap();
    due(&owed, "default grant owed");
    let granted = converged(converge_on(&owed.places, &code, &sync_here()));
    assert!(matches!(granted, FleetSync::Granted(_)), "{granted:?}");
    assert_eq!(checked(check_on(&owed.places, &code)), FleetCheck::Current);

    let unfinished = World::new("check-owed-unfinished");
    assert!(
        killed_at("book", || join(&unfinished.places, &by_hand(), &code)),
        "the join is cut short before its book names the issuer"
    );
    due(&unfinished, "did not finish");

    due(&other, "issues another fleet");

    let garbled = World::new("check-owed-garbled");
    aterm_uds::private_file::ensure_private_dir(&garbled.places.state).unwrap();
    write_private(&garbled.places.state, KEY_FILE, b"inert-not-a-key\n").unwrap();
    due(&garbled, "cannot be read");
}

#[test]
fn a_sync_leaves_a_key_it_cannot_read_as_it_is() {
    let (vaults, code, _serving) = issuing("sync-unread-vaults");
    let (_other, other_code, _other_serving) = issuing("sync-unread-other");
    let open = World::new("sync-unread-open");
    let garbled = World::new("sync-unread-garbled");
    for world in [&open, &garbled] {
        ok(join(&world.places, &by_hand(), &other_code));
    }
    let key = read_key_file(&open.places.state).unwrap();
    let path = open.places.state.join(KEY_FILE);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_private(&garbled.places.state, KEY_FILE, b"inert-not-a-key\n").unwrap();
    for (world, remedy) in [(&open, "`chmod 600` it"), (&garbled, LEAVE_THEN_ON)] {
        let config = world.config();
        let blocked = converged(converge_on(&world.places, &code, &sync_here()));
        assert!(
            matches!(&blocked, FleetSync::Blocked(why) if why.contains(remedy)),
            "{blocked:?}"
        );
        assert_eq!(world.config(), config, "nothing written");
        assert_eq!(synced(&world.places), None, "nothing recorded");
    }
    assert_eq!(
        std::fs::read(garbled.places.state.join(KEY_FILE)).unwrap(),
        b"inert-not-a-key\n"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        read_key_file(&open.places.state).unwrap(),
        key,
        "the key joined by hand stays"
    );
    assert!(
        read_peers(&vaults.places.state).unwrap().is_empty(),
        "the vault fleet's issuer was never dialled"
    );
}

#[test]
fn a_default_grant_a_pass_was_cut_short_before_is_written_by_the_next_one() {
    let (_issuer, code, _serving) = issuing("sync-owed");
    let member = World::new("sync-owed-member");
    let narrowed = World::new("sync-owed-narrowed");
    for world in [&member, &narrowed] {
        assert!(
            killed_at("grant", || {
                converge_on(&world.places, &code, &sync_here()).map(|synced| synced.report())
            }),
            "the pass is cut short at its grant"
        );
        let table = fleet_table(&world.config()).unwrap();
        assert_eq!(table.level(), Some(FleetScope::Content.word()));
        assert!(table.drive_from.is_empty(), "{table:?}");
        assert!(in_state(&world.places, GRANT_OWED_FILE).unwrap());
    }
    // The next pass finishes the join, and its grant with it.
    let joined = converged(converge_on(&member.places, &code, &sync_here()));
    assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(table.level(), Some(aterm_net::fleet::DRIVE_LEVEL));
    assert_eq!(table.drive_from, [aterm_net::fleet::ALL_MEMBERS]);
    assert!(!in_state(&member.places, GRANT_OWED_FILE).unwrap());
    assert_eq!(
        converged(converge_on(&member.places, &code, &sync_here())),
        FleetSync::Current
    );
    // A record an older aterm's pass left beside the content its join wrote is granted too.
    ok(allow(&member.places, &args(&["content"])));
    write_private(&member.places.state, GRANT_OWED_FILE, b"").unwrap();
    let granted = converged(converge_on(&member.places, &code, &sync_here()));
    assert!(matches!(granted, FleetSync::Granted(_)), "{granted:?}");
    let table = fleet_table(&member.config()).unwrap();
    assert_eq!(table.drive_from, [aterm_net::fleet::ALL_MEMBERS]);
    assert!(!in_state(&member.places, GRANT_OWED_FILE).unwrap());
    // A level the person sets once the pass was cut short stands.
    ok(allow(&narrowed.places, &args(&["content"])));
    assert!(!in_state(&narrowed.places, GRANT_OWED_FILE).unwrap());
    let config = narrowed.config();
    let joined = converged(converge_on(&narrowed.places, &code, &sync_here()));
    assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    let table = fleet_table(&narrowed.config()).unwrap();
    assert_eq!(
        (table.level(), table.drive_from.len()),
        (Some(FleetScope::Content.word()), 0),
        "the person's level stands: {config}"
    );
}

#[test]
fn a_sync_on_ends_fleet_off_under_the_books_lock() {
    let (_issuer, code, _serving) = issuing("sync-on-held");
    let member = World::new("sync-on-held-member");
    ok(join(&member.places, &by_hand(), &code));
    ok(leave(&member.places, &[]));
    let off = member.places.state.join(member::OFF_FILE);
    assert!(off.is_file());
    let held = member::hold_book(&member.places.state).unwrap();
    let busy = sync_with(&member.places, true, publishes(&code), &sync_here());
    assert!(
        matches!(&busy, Err(Failure::Failed(why)) if why.contains("another aterm is writing the book")),
        "{busy:?}"
    );
    assert!(
        off.is_file(),
        "fleet-off stands while another edit holds the lock"
    );
    drop(held);
    let joined = converged(sync_with(
        &member.places,
        true,
        publishes(&code),
        &sync_here(),
    ));
    assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    assert!(!off.exists());
}

#[test]
fn a_host_word_that_is_a_macs_short_name_binds_its_whole_node() {
    let world = World::new("short-names");
    member::ensure_private_dir(&world.places.state).unwrap();
    let joined = |byte: u8, host: &str, last: u8| Peer {
        node: Some(format!("{byte:02x}").repeat(16)),
        addr: Some(IpAddr::from([192, 168, 1, last])),
        via: Via::Joined,
        ..manual_peer(host.to_owned(), 7100)
    };
    let book = [
        joined(0x10, "m102-macbook-M4-Pro-24GB-500GB.local", 22),
        joined(0x11, "MacbookAir-m27-M5-32GB-4TB.local", 27),
        joined(0x12, "m1-Air-M2-24GB-1TB-2.local", 41),
        joined(0x14, "m8-mini-a.local", 8),
        joined(0x15, "m8-mini-b.local", 9),
        joined(0x16, "m22-Macbook-M5-Max-128GB-8TB.local", 30),
        joined(0x17, "lab-mini-a.local", 50),
        joined(0x18, "lab-mini-b.local", 51),
    ];
    let text: String = book
        .iter()
        .map(|peer| format!("{}\n", peer.line()))
        .collect();
    write_private(&world.places.state, PEERS_FILE, text.as_bytes()).unwrap();
    let node = |byte: u8| format!("{byte:02x}").repeat(16);
    let word = |what: &str| host_word(&world.places, what, None);
    assert_eq!(
        word("m27"),
        Ok(node(0x11)),
        "a machine number past the lead"
    );
    assert_eq!(word("MacbookAir"), Ok(node(0x11)), "the lead word");
    // A leading `m<N>` is every verb's word for its peer (`HostRank::LeadingMachine`): read as
    // typed, and bound as every verb binds it.
    for leading in ["m102", "m1", "m8", "m22"] {
        assert_eq!(word(leading), Ok(leading.to_owned()));
    }
    assert_eq!(
        word("m2"),
        Ok("m2".to_owned()),
        "a chip's M2: read as typed"
    );
    assert_eq!(word("zz-unknown"), Ok("zz-unknown".to_owned()));
    for whole in [
        "m102-macbook-M4-Pro-24GB-500GB.local",
        "m102-macbook-M4-Pro-24GB-500GB",
        "192.168.1.22",
    ] {
        assert_eq!(word(whole), Ok(whole.to_owned()), "read as before: {whole}");
    }
    let many = word("lab").expect_err("two Macs answer to lab");
    assert!(
        many.contains("lab-mini-a.local at 192.168.1.50:7100")
            && many.contains("lab-mini-b.local at 192.168.1.51:7100"),
        "{many}"
    );
    for (what, host) in [
        (node(0x11), "MacbookAir-m27-M5-32GB-4TB.local"),
        ("m102".to_owned(), "m102-macbook-M4-Pro-24GB-500GB.local"),
    ] {
        match bound_peer(&world.places, &what, None) {
            Ok(Bound::Unbound(peer)) => assert_eq!(peer.host, host),
            other => panic!("{what}: {other:?}"),
        }
    }
    let refused = bound_peer(&world.places, "m8", None).map_err(failure_words);
    assert!(
        refused
            .as_ref()
            .is_err_and(|why| why.contains(&node(0x14)) && why.contains(&node(0x15))),
        "{refused:?}"
    );
    // A short name of this machine's own entry names this machine.
    write_private(
        &world.places.state,
        member::NODE_FILE,
        format!("{}\n", node(0x11)).as_bytes(),
    )
    .unwrap();
    let own = word("m27").unwrap();
    assert_eq!(
        this_machine_alone(&world.places, &own),
        Ok(Some(node(0x11)))
    );
}

/// A short name is read only after every name a verb reads a peer or this machine by: a swarm
/// roster's label for a node, and this machine's `[fleet] machine`, each keep the word as typed,
/// so the tiers that bind it name the Mac they say, never a host whose words hold it.
#[test]
fn a_roster_label_or_this_machines_own_name_comes_before_a_short_name() {
    let world = World::new("short-after");
    member::ensure_private_dir(&world.places.state).unwrap();
    let node = |byte: u8| format!("{byte:02x}").repeat(16);
    let joined = |byte: u8, host: &str| Peer {
        node: Some(node(byte)),
        addr: Some(IpAddr::from([192, 168, 1, byte])),
        via: Via::Joined,
        ..manual_peer(host.to_owned(), 7100)
    };
    let book = [
        joined(0x11, "MacbookAir-m27-M5-32GB-4TB.local"),
        joined(0x17, "lab-mini-a.local"),
        joined(0x18, "Mac-Studio-m31-M4-Max.local"),
    ];
    let text: String = book
        .iter()
        .map(|peer| format!("{}\n", peer.line()))
        .collect();
    write_private(&world.places.state, PEERS_FILE, text.as_bytes()).unwrap();
    let roster = [member::PeerIdentity {
        node: Some(node(0x17)),
        label: Some("m27".to_owned()),
        ..member::PeerIdentity::default()
    }];
    let word = |what: &str, identities: &[member::PeerIdentity]| {
        host_word_among(&world.places, what, None, identities)
    };
    assert_eq!(word("m27", &[]), Ok(node(0x11)), "no roster: a short name");
    assert_eq!(
        word("m27", &roster),
        Ok("m27".to_owned()),
        "the roster's label for lab-mini-a: read as typed"
    );
    assert_eq!(
        member::named_with(&book, &[], &roster, "m27").map(|peer| peer.map(|p| p.host.clone())),
        Ok(Some("lab-mini-a.local".to_owned())),
        "which binds the node the roster labels"
    );

    // This machine's `[fleet] machine` names it before any host's words do.
    write_private(
        &world.places.state,
        member::NODE_FILE,
        format!("{}\n", node(0x20)).as_bytes(),
    )
    .unwrap();
    assert_eq!(
        word("m31", &[]),
        Ok(node(0x18)),
        "no machine name: a short name"
    );
    std::fs::create_dir_all(world.places.config.parent().unwrap()).unwrap();
    std::fs::write(&world.places.config, "[fleet]\nmachine = \"m31\"\n").unwrap();
    assert_eq!(word("m31", &[]), Ok("m31".to_owned()));
    assert_eq!(
        this_machine_alone(&world.places, "m31"),
        Ok(Some(node(0x20))),
        "and the drive refuses it as this machine"
    );
}

/// A short name that names this machine's issuer waits for a copy of its book, as every word
/// that names it loosely does: a member that joined since this machine last read it may answer
/// to the word too.
#[test]
fn a_short_name_of_the_issuer_is_refused_while_no_book_here_lists_its_members() {
    let world = World::new("short-issuer");
    member::ensure_private_dir(&world.places.state).unwrap();
    let node = |byte: u8| format!("{byte:02x}").repeat(16);
    let peer = |byte: u8, host: &str, via: Via| Peer {
        node: Some(node(byte)),
        addr: Some(IpAddr::from([192, 168, 1, byte])),
        via,
        ..manual_peer(host.to_owned(), 7100)
    };
    let book = [
        peer(0x11, "MacbookAir-m27-M5-32GB-4TB.local", Via::Issuer),
        peer(0x10, "m102-macbook-M4-Pro-24GB-500GB.local", Via::Joined),
    ];
    let text: String = book
        .iter()
        .map(|peer| format!("{}\n", peer.line()))
        .collect();
    write_private(&world.places.state, PEERS_FILE, text.as_bytes()).unwrap();
    let unread = Missed {
        why: "its issuer's book could not be read".to_owned(),
        silent: true,
        unserved: false,
    };
    let word = |what: &str, unread: Option<&Missed>| host_word(&world.places, what, unread);

    // No copy held: the issuer's short names stay as typed, and binding one is refused.
    for short_name in ["MacbookAir", "m27"] {
        assert_eq!(word(short_name, None), Ok(short_name.to_owned()));
        let refused = bound_peer(&world.places, short_name, None).map_err(failure_words);
        assert!(
            refused.as_ref().is_err_and(|why| why.starts_with(&format!(
                "no book here says which members `{short_name}` names beside this machine's \
                 issuer"
            ))),
            "{refused:?}"
        );
    }
    assert_eq!(
        word("m102", None),
        Ok("m102".to_owned()),
        "a member's leading m<N> is read as typed, as every verb reads it"
    );
    assert_eq!(
        word(&node(0x11), None),
        Ok(node(0x11)),
        "its node id names it alone"
    );

    // A copy held: the short name binds the issuer by its node id, unless the read before it
    // brought none to keep.
    write_private(
        &world.places.state,
        ISSUER_BOOK_FILE,
        format!("issuer node={}\n", node(0x11)).as_bytes(),
    )
    .unwrap();
    assert_eq!(word("MacbookAir", None), Ok(node(0x11)));
    assert_eq!(word("m27", None), Ok(node(0x11)));
    assert_eq!(word("m27", Some(&unread)), Ok("m27".to_owned()));
}

/// Every member drives every member, a vault admin included (docs/DESIGN-fluid-swarm-2026-10-28.md
/// F2, which retired C19a's exemption once `Approval::Open` admitted nobody by asking): a Mac
/// that joins by sync gets `[fleet] allow = "drive"`, `drive_from = ["*"]` and owes nothing,
/// following a rotated fleet code keeps it, an unfinished pass's `content` beside
/// `fleet-sync-grant` is widened to drive (never returned to `facts`), and a person's
/// `aterm fleet allow facts` stands across later syncs.
/// NEGATIVE CONTROL: restoring the exemption (a synced join owes no grant and keeps `facts`)
/// reads `facts`.
#[test]
fn an_admin_that_joins_by_sync_gets_drive_from_every_member() {
    let (issuer, first_code, _first_serving) = issuing("sync-admin-first");
    let (second, second_code, _second_serving) = issuing("sync-admin-second");
    let admin = World::new("sync-admin-steward");
    assert!(!admin.places.state.join(CODE_FILE).exists());
    let drives = |world: &World| {
        let table = fleet_table(&world.config()).unwrap();
        assert_eq!(
            table.level(),
            Some(aterm_net::fleet::DRIVE_LEVEL),
            "{table:?}"
        );
        assert_eq!(
            table.drive_from,
            [aterm_net::fleet::ALL_MEMBERS],
            "{table:?}"
        );
    };

    let joined = converged(sync_with(
        &admin.places,
        false,
        publishes_for(&first_code),
        &sync_here(),
    ));
    assert!(matches!(joined, FleetSync::Joined(_)), "{joined:?}");
    drives(&admin);
    assert!(!in_state(&admin.places, GRANT_OWED_FILE).unwrap());
    assert_eq!(
        read_key_file(&admin.places.state).unwrap(),
        read_key_file(&issuer.places.state).unwrap()
    );
    assert_eq!(
        checked(check_with(&admin.places, publishes_for(&first_code))),
        FleetCheck::Current
    );

    // Following a rotated fleet code keeps every member's drive.
    let followed = converged(sync_with(
        &admin.places,
        false,
        publishes_for(&second_code),
        &sync_here(),
    ));
    assert!(matches!(followed, FleetSync::Joined(_)), "{followed:?}");
    assert_eq!(
        read_key_file(&admin.places.state).unwrap(),
        read_key_file(&second.places.state).unwrap()
    );
    drives(&admin);

    // An older build's unfinished pass left `content` beside the owed-grant record: sync writes
    // the grant it owed.
    ok(allow(&admin.places, &args(&["content"])));
    write_private(&admin.places.state, GRANT_OWED_FILE, b"").unwrap();
    assert!(matches!(
        checked(check_with(&admin.places, publishes_for(&second_code))),
        FleetCheck::Due(_)
    ));
    let granted = converged(sync_with(
        &admin.places,
        false,
        publishes_for(&second_code),
        &sync_here(),
    ));
    assert!(matches!(granted, FleetSync::Granted(_)), "{granted:?}");
    assert!(!in_state(&admin.places, GRANT_OWED_FILE).unwrap());
    drives(&admin);

    // A person's narrower level stands across later syncs.
    ok(allow(&admin.places, &args(&["facts"])));
    let narrowed = admin.config();
    assert_eq!(
        converged(sync_with(
            &admin.places,
            false,
            publishes_for(&second_code),
            &sync_here(),
        )),
        FleetSync::Current
    );
    assert_eq!(
        admin.config(),
        narrowed,
        "a person's facts on an admin stands across sync"
    );
}
/// `[fleet] machine` in `aterm.toml` identifies this Mac's own node for `--host` / `peers rm`
/// without matching unrelated labels, and survives `join`, `allow` and `leave`.
/// NEGATIVE CONTROL: omitting `[fleet] machine` from `names_this_machine` fails to recognize `m32`
/// as this Mac on a stock-named machine.
#[test]
fn fleet_machine_in_aterm_toml_names_this_machine_and_survives_join_allow_and_leave() {
    let (_issuer, code, _serving) = issuing("fleet-machine-issuer");
    let member = World::new("fleet-machine-member");
    std::fs::create_dir_all(member.places.config.parent().unwrap()).unwrap();
    std::fs::write(&member.places.config, "[fleet]\nmachine = \"m32\"\n").unwrap();
    ok(join(&member.places, &sync_here(), &code));

    let own = own_node(&member.places).expect("joined node");
    assert_eq!(names_this_machine(&member.places, "m32"), Some(own.clone()));
    assert_eq!(names_this_machine(&member.places, "M32"), Some(own));
    assert_eq!(names_this_machine(&member.places, "m33"), None);

    ok(allow(&member.places, &args(&["content"])));
    assert_eq!(
        fleet_table(&member.config()).unwrap().machine.as_deref(),
        Some("m32")
    );
    ok(leave(&member.places, &[]));
    assert_eq!(
        fleet_table(&member.config()).unwrap().machine.as_deref(),
        Some("m32")
    );
}

#[test]
fn swarm_enabled_false_keeps_fleet_sync_idle_and_leaves_state_untouched() {
    let (_issuer, code, _serving) = issuing("swarm-off-issuer");
    let member = World::new("swarm-off-member");
    member.write_config("[swarm]\nenabled = false\n");
    assert_eq!(
        checked(check_with(&member.places, publishes_for(&code))),
        FleetCheck::Idle(FleetSync::SwarmOff)
    );
    assert_eq!(
        converged(sync_with(
            &member.places,
            false,
            publishes_for(&code),
            &sync_here(),
        )),
        FleetSync::SwarmOff
    );
    assert!(
        read_key_file(&member.places.state).is_err(),
        "no fleet key written while [swarm] enabled = false"
    );
    // Negative control: removing `[swarm] enabled = false` joins on the next sync.
    member.write_config("");
    assert!(matches!(
        converged(sync_with(
            &member.places,
            false,
            publishes_for(&code),
            &sync_here(),
        )),
        FleetSync::Joined(_)
    ));
}

#[test]
fn fleet_leave_stops_and_removes_installed_node_job_without_recording_node_off() {
    use crate::node_service::{self, ServiceSupervisor};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    struct FakeNodeSupervisor {
        stopped: Mutex<Vec<PathBuf>>,
    }
    impl ServiceSupervisor for FakeNodeSupervisor {
        fn unit_file(&self, home: &Path) -> PathBuf {
            node_service::plist_path(home)
        }
        fn render_unit(&self, program: &Path, home: &Path, state_dir: &Path) -> String {
            node_service::node_plist_text(program, home, state_dir, None)
        }
        fn running_pid(&self, _home: &Path) -> Result<Option<u32>, String> {
            Ok(Some(777))
        }
        fn load_and_start(&self, _unit_file: &Path) -> Result<(), String> {
            Ok(())
        }
        fn stop_and_unload(&self, unit_file: &Path) -> Result<(), String> {
            self.stopped.lock().unwrap().push(unit_file.to_path_buf());
            Ok(())
        }
    }

    let (_issuer, code, _serving) = issuing("leave-node-job-issuer");
    let member = World::new("leave-node-job-member");
    ok(join(&member.places, &sync_here(), &code));

    let home = member.places.state.parent().unwrap().join("home");
    let exe = home.join("aterm");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(&exe, b"bin").unwrap();
    let sup = FakeNodeSupervisor {
        stopped: Mutex::new(Vec::new()),
    };
    let unit = node_service::install_with(&exe, &home, &member.places.state, false, &sup).unwrap();
    assert!(unit.is_file());

    let report = ok(leave_within_node(
        &member.places,
        Some(&home),
        Some(&sup),
        Duration::from_millis(20),
    ));
    assert!(
        report.contains(&format!("  removed    {} (the node job)\n", unit.display())),
        "report={report}"
    );
    assert!(!unit.exists(), "unit file must be unlinked on fleet leave");
    assert_eq!(*sup.stopped.lock().unwrap(), vec![unit]);
    assert!(
        !node_service::node_turned_off(&member.places.state),
        "fleet leave must write fleet-sync-off, not node-off, so a future join can install sync.node"
    );

    // Negative control: leaving again when neither fleet files nor node job remain says in no fleet.
    let again = ok(leave_within_node(
        &member.places,
        Some(&home),
        Some(&sup),
        Duration::from_millis(20),
    ));
    assert!(
        again.starts_with("aterm fleet leave: this machine is in no fleet\n"),
        "again={again}"
    );
}
