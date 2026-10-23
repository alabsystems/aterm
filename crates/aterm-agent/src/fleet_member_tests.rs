// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm fleet init|join|peers|allow|leave` against scratch state directories and config files, with the issuer a real fleet listener on loopback.

use super::*;
use aterm_net::fleet::{FleetEvent, Listening, read_key_file, serve};
use aterm_net::fleet_member::{Issuer, read_peers, serve_book, serve_join};
use std::net::{Ipv4Addr, TcpListener};
use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A wait for something that must happen: a hang detector, never a budget.
const HANG: Duration = Duration::from_secs(60);

/// One machine's scratch world: its state directory and aterm.toml.
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

/// A real code, its check characters whole.
fn a_code() -> String {
    JoinCode {
        secret: [7; member::SECRET_LEN],
        host: "m7.local".to_owned(),
        port: 7100,
    }
    .text()
}

/// THE CODE IS NEVER AN ARGUMENT: `join` refuses a code in any argument — a
/// whole one, one pasted without its `atf1-`, part of one, one behind a
/// made-up flag or as a flag's value — and says why, while its own flags
/// pass; and the refusal comes before `--help` is read.
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

/// A code is read from its file's first line that has anything on it, and
/// is known to have come from a file (whose copy the join then says to
/// delete); a code typed at the terminal leaves nothing behind to name.
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

/// `[fleet] listen` and `allow` are set and removed IN PLACE: every other
/// line — a comment, another table, `[[fleet.peers]]` — is kept, a `[fleet]`
/// already there is edited where it stands, the dotted and inline spellings
/// are edited too, and a file that is not TOML or a `fleet` that is no table
/// is left alone with the reason.
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

/// The issuer as a real fleet listener on `listener`, serving `join` into
/// `state` with the key `init` wrote there, and its book from `state`;
/// stopped on drop.
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

    /// [`Listener::serve`] serving only `serves`: without `book`, an older
    /// aterm.
    fn serving(listener: TcpListener, state: &Path, serves: &'static [FleetOp]) -> Self {
        Self::naming(listener, state, serves, None)
    }

    /// [`Listener::serve`], its survey's header naming `claim` in place of
    /// its own node — what it proves is still its own.
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
        let state = state.to_path_buf();
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
                                &state,
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

/// A loopback port nothing listens on.
fn closed_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    listener.local_addr().unwrap().port()
}

fn ok(result: Result<String, Failure>) -> String {
    match result {
        Ok(report) => report,
        Err(failure) => panic!("{failure:?}"),
    }
}

/// INIT, JOIN, PEERS END TO END: `init` writes the key, the code (whose path
/// it prints, never the code) and `[fleet]`; `join` with that code writes the
/// same key and its own `[fleet]` and registers with the issuer — a real
/// fleet listener on loopback — which records the member by node id; the
/// member records the issuer; and each side's `peers` checks the other,
/// giving the member, whose aterm is not running, its reason.
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
    ok(join(&member.places, &opts, &code));
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

/// A MACHINE THAT TAKES A MEMBER'S NODE ID IS REFUSED, NOT RECORDED AS IT:
/// one holding the member's node id under a key of its own (a copied
/// `fleet-node`, or the member itself after it lost its node key) joins with
/// the fleet's code, and the issuer refuses it, naming `aterm fleet peers rm
/// <node>` on the issuer; nothing of the fleet is written there, and the
/// member's entry stands, bound to its own key.
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
    member::write_private(
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

/// A JOIN AGAIN OF THE SAME FLEET REACHES ONLY THE ISSUER THIS MAC BINDS:
/// another holder of the fleet's secret — an issuer of its own holding this
/// fleet's key — answering at the host and port a code names, proving no
/// node or a node of its own, is refused before anything is sent it, naming
/// `aterm fleet leave` as the way to a new issuer; the member's key, book
/// and `[fleet]` stay, its issuer the one it joined.
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
    member::write_key_file(&other.places.state, &secret).unwrap();
    member::remove_private(&other.places.state, NODE_KEY_FILE).unwrap();
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

/// AN ISSUER THAT LOST ITS NODE KEY IS BOUND AGAIN AS ITS REFUSAL SAYS: a join
/// again of the same fleet reaching the bound issuer while it proves no node
/// (its node key gone) is refused, nothing written here, and the refusal
/// names that issuer's remedy — `aterm fleet init` there, then `peers rm` of
/// it here — beside `leave`; once `init` there mints a key, the join is
/// refused as the bound node under another key, and after the `peers rm`
/// here it binds the new key.
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

    member::remove_private(&issuer.places.state, NODE_KEY_FILE).unwrap();
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

/// A JOIN THE ISSUER RECORDED THAT CANNOT WRITE aterm.toml ([`unfinished`];
/// the review of fleet.1-m2): a config the edit refuses is refused before
/// the dial — the issuer's book holds no member and nothing of the fleet is
/// written here — while one whose edit reads back but whose write then fails
/// (the config service's write lock, `.aterm.toml.aterm-write.lock`, is no
/// file) fails after the issuer recorded the machine: the issuer lists it and
/// this machine holds the key and the issuer, but has no `[fleet] listen`.
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
    assert_eq!(
        read_peers(&member.places.state).unwrap()[0].via,
        Via::Issuer
    );
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
    assert_eq!(read_peers(&issuer.places.state).unwrap().len(), 1);
}

/// `--rotate` mints a new secret and keeps the host and port the fleet's
/// code named, and THE ROSTER: every member it recorded stays in the book —
/// each now holds the old secret until it joins again, which its check then
/// says — and the report names each, so the person can work through them and
/// take out the machine that was lost.
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

    member::remove_private(&issuer.places.state, CODE_FILE).unwrap();
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

/// A `[[fleet.peers]]` ENTRY FOR A MACHINE IN THE BOOK IS ONE PEER: by the
/// bare name a person types for the `.local` one it joined as, or by the
/// address it registered from; another machine's name, or another port, is
/// a peer of its own.
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

/// AN UNPROVEN CLAIM NEVER COLLAPSES INTO A PROVEN ANSWER: this Mac read
/// through a place its book names by hand (proven, its own node: the row the
/// menu bar's glance drops as this Mac) and a Mac at a later place that
/// claims this Mac's node without its key are two blocks, so the claimant is
/// listed under its mark and never vanishes with this Mac's row; and so are
/// a member's checked answer and a claim of its node at a place a person
/// named.
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

/// A MAC THAT DID NOT ANSWER IS ONE ROW AT ITS PLACE: a person's `lab-mini`
/// (no node) and the copy of the `lab-mini.local` that joined there, read
/// side by side and both failing the same way at the address both tried,
/// are that node's failure once — what the menu bar's "not answering" lists.
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

/// A MEMBER'S `peers` READS ITS ISSUER'S BOOK: a peer the issuer's person
/// named is listed on the member, which never named it, as a copy of the
/// issuer's book, checked like the rest, and kept in a file of its own —
/// never in the member's book, which keeps only the words an older aterm
/// reads; `rm` of the copy on the member is refused and changes nothing;
/// taken out on the issuer, the copy goes at the member's next read.
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
    member::write_issuer_book(&member.places.state, &issuer_node, &stale).unwrap();
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

/// A PROVEN MAC THAT NAMES THIS ONE IN ITS SURVEY IS LISTED AS REFUSED,
/// never read as this Mac: an issuer that proves the key this member binds
/// it to and whose survey's header names the member's own node — which the
/// menu bar's glance would drop as this Mac answering through its book — is
/// the read's row with a reason naming both nodes, and nothing it sent is
/// passed on.
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

/// WHAT A READ IS SHORT OF ITS ISSUER'S BOOK: an issuer that answered with
/// no book leaves a read that asked its copy of the last book, or — holding
/// none (a first read, or one after a refusal took it away) — only the peers
/// this machine knows; an older issuer, or one that shares none, the same
/// peers; a book that came, or an issuer not reached, nothing.
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

/// A PEER A PERSON NAMED IN aterm.toml STANDS FOR A COPY AT ITS PLACE: the
/// issuer's book holds the place the member's `[[fleet.peers]]` names, so
/// the member reads it once, as its person's entry (VIA `config`) — never
/// as the copy — and when the issuer's book drops it, the member's next read
/// still has its row.
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

/// `peers add` records a peer once and `rm` takes it out; `leave` removes the
/// key, the code, the book and `[fleet]`'s keys — on the issuer, saying its
/// fleet went with it — and says when there is nothing to leave.
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

/// A CODE WHOSE ISSUER IS PAST THE LAN is never dialled, and the join's
/// failure says so by its kind — no aterm to start, no consent to give — and
/// writes nothing of the fleet.
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

/// EVERY REASON A FLEET READ GIVES FITS ITS ROW WHOLE: for a LAN peer at
/// the longest IPv4 address, named by a Mac's usual `.local` name, with the
/// longest deadline, each reason a peer's row carries is at most what the
/// row keeps whole beside any name (`aterm_ctl::UNANSWERED_REASON_CHARS`),
/// so its remedy — at its end — is never cut off.
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

/// A FAILED JOIN IS ADVISED BY ITS KIND, the first that fits: a refusal
/// from a machine that issues no fleet, an issuer past the LAN, a connect
/// refused to this process (`EPERM`, `EACCES`: a sandbox, a firewall), no
/// route to the host (on a Mac, local-network privacy), none to its network
/// (offline, a VPN), a machine that claims a node this one binds to a key
/// without proving that key, another node answering for the issuer a join
/// again of the same fleet must reach, or a machine proving none there (which
/// may be that issuer without its key), an issuer that binds this machine's
/// node to another, an issuer that recorded this machine from another address
/// before keys came (no key bound: not the advice of one bound to another).
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

/// A PERSON'S ENTRY AT A COPY'S PLACE IS READ BESIDE IT: a read dials
/// `[[fleet.peers]] host = "m7"` and the copy of the issuer's book at
/// `m7.local` that names a node, both — the place is what that node's
/// registrant said — and what answers either as that node is held to the
/// copy's key.
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
    member::write_issuer_book(&world.places.state, &issuer_node, &[copy.clone(), by_hand]).unwrap();
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

/// AN ANSWER THAT PROVED NO BOUND NODE KEY IS UNPROVEN in the survey: its
/// node is its claim.
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

/// `peers --timeout` takes seconds, more than none and at most the survey's
/// longest; anything else is a usage error, as `peers` with a stray word is.
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

/// `aterm fleet allow`: a level this machine's person sets here alone.
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

/// A JOIN OF ANOTHER FLEET KEEPS NO DRIVE GRANT: the grants named the nodes
/// of the fleet this machine held, so a join under another secret takes
/// them out and a drive level reads as content.
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

/// A JOIN OF ANOTHER FLEET THAT FAILS AFTER ITS ISSUER RECORDED IT FINISHES
/// ON A JOIN AGAIN WITH THAT CODE ([`unfinished`]): a member whose book binds
/// its issuer joins another fleet's code while its book cannot be read (a
/// directory in its place).
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
    let failed = match join(&member.places, &opts, &other_code) {
        Err(Failure::Failed(why)) => why,
        other => panic!("{other:?}: its book cannot be read"),
    };
    assert!(failed.contains("has recorded this machine"), "{failed}");
    assert_eq!(read_peers(&other.places.state).unwrap().len(), 1);
    std::fs::remove_dir(&book).unwrap();
    member::write_private(&member.places.state, PEERS_FILE, &held).unwrap();
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

/// A JOIN OF ANOTHER FLEET WHOSE KEY CANNOT BE WRITTEN LEAVES THE BOOK AS IT
/// WAS ([`unfinished`]): a member whose book binds its issuer joins another
/// fleet's code while its key cannot be replaced (a directory in its
/// place).
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
    assert_eq!(
        read_peers(&member.places.state).unwrap(),
        book_before,
        "the old fleet's book, its issuer bound"
    );

    std::fs::remove_dir(&key).unwrap();
    member::write_private(&member.places.state, KEY_FILE, &held).unwrap();
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

/// A MEMBER'S DRIVE GRANT FOLLOWS ITS ISSUER'S BOOK (the step-in): a node the
/// issuer takes out drives the member no more once the member reads that
/// book — `peers`, or `survey --fleet` — and a join again under its old key
/// does not bring the grant back; a node the issuer binds afresh to another
/// key the same.
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

/// A PERSON'S ENTRY NEVER HIDES THE NODE A KEY BINDS AT ITS PLACE: on a
/// member whose aterm.toml names a Mac by hand (`[[fleet.peers]]`) at the
/// place its copy of the issuer's book holds that Mac's node — an entry a
/// read dials beside the copy — `allow drive --from` grants that node,
/// named by the place, the copy's host, the node id, or the `host:port` of an
/// entry that spells the copy's host another way (`lab-mini.invalid:<port>`
/// for the copy's `lab-mini`: the same machine, and a word only that entry
/// answers to), with the key its copy binds, as the drive's admission reads
/// it; `aterm ctl --host` dials it by its node id or that entry's
/// `host:port`; and `aterm ctl survey --host` reads it by the place, the
/// copy's host or the node id, which the person's entries answer to as well.
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
