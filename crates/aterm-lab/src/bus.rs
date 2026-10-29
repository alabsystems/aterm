// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Guarded lab fabric broker in scratch state: its layout ([`lay`]), the proofs' unjailed broker
//! ([`Fabric::start`]) and the wait for a broker's socket both share ([`await_listening`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_uds::spawnfd::Stdio;

/// The fleet a lab's bus names (`/f/lab/…`).
pub const FLEET: &str = "lab";

/// How long a broker the lab started may take to listen: a hang detector, never a latency
/// budget ([`await_listening`]).
pub const LISTEN_HANG: Duration = Duration::from_secs(60);

/// A mint: `aterm link mint <grant> --secret-file <secret>`'s argv in, its standard output (one
/// `<grant> <tag-hex>` line) out, or why it failed ([`lay`]).
pub type Mint<'a> = dyn FnMut(&[String]) -> Result<Vec<u8>, String> + 'a;

/// A lab's bus laid out ([`lay`]): the keys a bridge and an observer read, and the live dir its
/// broker and bridge write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Laid {
    /// The keys dir (0700): the program link, the mint secret and the caps.
    pub keys: PathBuf,
    /// `<keys>/aterm`, a link to the aterm under test (the broker's and the bridge's program).
    pub program: PathBuf,
    /// `<keys>/mint.secret` (32 bytes, 0600).
    pub secret: PathBuf,
    /// `<keys>/node.cap` (0600): the node's ring, [`aterm_link::enable::node_grants`] in order.
    pub cap: PathBuf,
    /// `<keys>/observer.cap` (0600): an observer's ring, [`aterm_link::enable::person_grants`],
    /// when one was asked for.
    pub observer: Option<PathBuf>,
    /// The live dir (0700): the broker's socket and log, the bridge's state.
    pub live: PathBuf,
    /// `<live>/b.sock`, the broker's socket.
    pub sock: PathBuf,
    /// `<live>/state` (0700), the bridge's `--state` dir, holding `node`.
    pub state: PathBuf,
    /// The node id (`n-` and 16 hex digits).
    pub node: String,
    /// The `[fabric]` table the instance's `aterm.toml` gets.
    pub table: String,
}

impl Laid {
    /// The bridge's argv, as the table's `command` names it.
    #[must_use]
    pub fn command(&self) -> Vec<String> {
        [
            self.program.to_string_lossy().as_ref(),
            "link",
            "serve",
            "--fleet",
            FLEET,
            "--broker",
            self.sock.to_string_lossy().as_ref(),
            "--cap-file",
            self.cap.to_string_lossy().as_ref(),
            "--state",
            self.state.to_string_lossy().as_ref(),
            "--accept-from",
            &self.node,
        ]
        .map(str::to_string)
        .to_vec()
    }

    /// The broker's argv: `<program> link broker <sock> <live>/b.log --secret-file <secret>`.
    #[must_use]
    pub fn broker_argv(&self) -> Vec<String> {
        [
            self.program.to_string_lossy().as_ref(),
            "link",
            "broker",
            self.sock.to_string_lossy().as_ref(),
            self.live.join("b.log").to_string_lossy().as_ref(),
            "--secret-file",
            self.secret.to_string_lossy().as_ref(),
        ]
        .map(str::to_string)
        .to_vec()
    }

    /// What `fabric.toml` records of this bus, for `aterm link ls` and the other readers of the
    /// rendezvous file ([`aterm_link::enable::Rendezvous`]).
    #[must_use]
    pub fn rendezvous(&self) -> aterm_link::enable::Rendezvous {
        aterm_link::enable::Rendezvous {
            fleet: FLEET.to_string(),
            broker: self.sock.to_string_lossy().into_owned(),
            node: self.node.clone(),
            cap_file: self.cap.to_string_lossy().into_owned(),
            state: self.state.to_string_lossy().into_owned(),
            command: self.command(),
        }
    }
}

/// Lay a bus out: the keys under `keys` (the program link, a 32-byte mint secret, `node.cap`
/// minted from [`aterm_link::enable::node_grants`] in order, and with `observer` an
/// `observer.cap` from [`aterm_link::enable::person_grants`]), the bridge's state with its node id
/// under `live`, each dir 0700 and each secret or cap 0600. Every grant goes through `mint`.
/// Nothing is started.
/// # Errors
/// A file could not be made, `observer` is no person id, a word of the bridge's command is not
/// plain ([`aterm_link::enable::argv_safe`]), or a grant could not be minted (naming it; no cap
/// is written then).
pub fn lay(
    aterm: &Path,
    keys: &Path,
    live: &Path,
    observer: Option<&str>,
    mint: &mut Mint<'_>,
) -> Result<Laid, String> {
    use std::os::unix::fs::PermissionsExt as _;
    if let Some(who) = observer
        && !aterm_link::person::is_person_id(who)
    {
        return Err(format!(
            "the bus's observer {who} is no person id (h-… or a-…)"
        ));
    }
    let state = live.join("state");
    for d in [keys, state.as_path()] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    for d in [keys, live, state.as_path()] {
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut random = [0u8; 40];
    {
        use std::io::Read as _;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut random))
            .map_err(|e| format!("/dev/urandom: {e}"))?;
    }
    let secret = keys.join("mint.secret");
    let write_private = |path: &Path, bytes: &[u8]| {
        std::fs::write(path, bytes)
            .and_then(|()| std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)))
            .map_err(|e| format!("{}: {e}", path.display()))
    };
    write_private(&secret, &random[..32])?;
    let node = format!(
        "n-{}",
        random[32..]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    );
    write_private(&state.join("node"), format!("{node}\n").as_bytes())?;
    let program = keys.join("aterm");
    std::os::unix::fs::symlink(aterm, &program)
        .map_err(|e| format!("{}: {e}", program.display()))?;
    let mut laid = Laid {
        keys: keys.to_path_buf(),
        cap: keys.join("node.cap"),
        observer: None,
        live: live.to_path_buf(),
        sock: live.join("b.sock"),
        state,
        table: String::new(),
        program,
        secret,
        node,
    };
    let command = laid.command();
    for word in &command {
        aterm_link::enable::argv_safe(word)?;
    }
    laid.table = format!("[fabric]\ncommand = \"{}\"\n", command.join(" "));
    let mut minted = |grants: Vec<String>| -> Result<Vec<u8>, String> {
        let mut caps = Vec::new();
        for grant in grants {
            let argv = [
                laid.program.to_string_lossy().as_ref(),
                "link",
                "mint",
                &grant,
                "--secret-file",
                laid.secret.to_string_lossy().as_ref(),
            ]
            .map(str::to_string);
            caps.extend(mint(&argv).map_err(|e| format!("aterm link mint {grant}: {e}"))?);
        }
        Ok(caps)
    };
    let node_caps = minted(aterm_link::enable::node_grants(FLEET, &laid.node))?;
    let observer_caps = observer
        .map(|who| minted(aterm_link::enable::person_grants(FLEET, who)))
        .transpose()?;
    write_private(&laid.cap, &node_caps)?;
    if let Some(caps) = observer_caps {
        let path = keys.join("observer.cap");
        write_private(&path, &caps)?;
        laid.observer = Some(path);
    }
    Ok(laid)
}

/// Wait for a broker's socket at `sock`: a hang detector of `hang` ([`LISTEN_HANG`]), never a
/// latency budget.
/// # Errors
/// The broker exited first (`gone`), or `hang` passed: naming what it said (`said`).
pub fn await_listening(
    sock: &Path,
    hang: Duration,
    mut gone: impl FnMut() -> bool,
    said: impl FnOnce() -> String,
) -> Result<(), String> {
    let end = Instant::now() + hang;
    while !sock.exists() {
        if Instant::now() >= end || gone() {
            return Err(format!(
                "the lab's broker never listened: {}",
                said().trim()
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// A lab's bus: its broker, the instance's `[fabric]` table and the bridge's state dir.
pub struct Fabric {
    broker: Option<aterm_uds::spawnfd::Child>,
    /// The broker's lifeline (`aterm_uds::lifeline`): it ends itself once this is
    /// dropped or the lab dies, a SIGKILL included, instead of serving on under
    /// launchd.
    _lifeline: aterm_uds::lifeline::Lifeline,
    /// The `[fabric]` table the instance's `aterm.toml` gets.
    pub table: String,
    /// The bridge's `--state` dir (its `topics/<sid>` cursors among it).
    pub state: PathBuf,
}

impl Fabric {
    /// Lay the bus out under `dir` ([`lay`], keys and live dir alike) and start its broker with
    /// `aterm`'s own `link broker`.
    /// # Errors
    /// A file could not be made, a grant could not be minted, or the broker never listened (within
    /// a minute: a hang detector).
    pub fn start(aterm: &Path, dir: &Path) -> Result<Fabric, String> {
        let laid = lay(aterm, dir, dir, None, &mut |argv: &[String]| {
            let out = aterm_uds::spawnfd::command(&argv[0])
                .args(&argv[1..])
                .stdin(Stdio::null())
                .output()
                .map_err(|e| e.to_string())?;
            if out.status.success() {
                Ok(out.stdout)
            } else {
                Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
            }
        })?;
        let said = dir.join("broker.out");
        let log = std::fs::File::create(&said).map_err(|e| format!("broker.out: {e}"))?;
        let err = log.try_clone().map_err(|e| format!("broker.out: {e}"))?;
        let argv = laid.broker_argv();
        let mut broker = aterm_uds::spawnfd::command(&argv[0]);
        broker
            .args(&argv[1..])
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(err));
        let lifeline = aterm_uds::lifeline::Lifeline::arm(&mut broker, dir)
            .map_err(|e| format!("the broker's lifeline in {}: {e}", dir.display()))?;
        let broker = broker
            .spawn()
            .map_err(|e| format!("aterm link broker: {e}"))?;
        let mut fabric = Fabric {
            broker: Some(broker),
            _lifeline: lifeline,
            table: laid.table,
            state: laid.state,
        };
        let listened = await_listening(
            &laid.sock,
            LISTEN_HANG,
            || {
                fabric
                    .broker
                    .as_mut()
                    .is_some_and(|b| matches!(b.try_wait(), Ok(Some(_))))
            },
            || std::fs::read_to_string(&said).unwrap_or_default(),
        );
        if let Err(e) = listened {
            fabric.stop();
            return Err(e);
        }
        Ok(fabric)
    }

    /// Append [`Fabric::table`] to the `aterm.toml` at `config`.
    /// # Errors
    /// The file could not be opened or written.
    pub fn name_in(&self, config: &Path) -> Result<(), String> {
        use std::io::Write as _;
        std::fs::OpenOptions::new()
            .append(true)
            .open(config)
            .and_then(|mut f| f.write_all(self.table.as_bytes()))
            .map_err(|e| format!("cannot name the bridge in {}: {e}", config.display()))
    }

    /// End the broker (`SIGTERM`, then `SIGKILL`).
    pub fn stop(&mut self) {
        let Some(mut broker) = self.broker.take() else {
            return;
        };
        if !matches!(broker.try_wait(), Ok(None)) {
            return;
        }
        if let Ok(pid) = i32::try_from(broker.id()) {
            let _ = crate::sys::kill(pid, libc::SIGTERM);
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                if !matches!(broker.try_wait(), Ok(None)) {
                    return;
                }
                if Instant::now() >= end {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = broker.kill();
        let _ = broker.wait();
    }
}

impl Drop for Fabric {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    /// The fake mint: `<grant> <tag>` for each grant, the tag a counter, failing on the grant
    /// numbered `fail_at` (from 1).
    fn fake_mint(fail_at: Option<usize>) -> impl FnMut(&[String]) -> Result<Vec<u8>, String> {
        let mut n = 0;
        move |argv: &[String]| {
            n += 1;
            assert_eq!(argv[1..3], ["link", "mint"], "{argv:?}");
            assert_eq!(argv[4], "--secret-file", "{argv:?}");
            if fail_at == Some(n) {
                return Err("refused QZP6WORD1".to_string());
            }
            Ok(format!("{} {n:04x}\n", argv[3]).into_bytes())
        }
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn grants_of(cap: &Path) -> Vec<String> {
        std::fs::read_to_string(cap)
            .unwrap()
            .lines()
            .map(|l| l.rsplit_once(' ').unwrap().0.to_string())
            .collect()
    }

    /// A LAB BUS LAYS THE NODE RING AND AN OBSERVER RING: the keys (program link, 32-byte secret,
    /// `node.cap` with the node's eight grants in order, `observer.cap` with an `a-` service ring)
    /// in one dir and the bridge's state in another, dirs 0700 and secrets 0600; the table's
    /// command is plain words naming the live socket, the node cap, the live state and the node;
    /// the rendezvous file renders and parses back with that command. A failed mint makes no cap
    /// and names its grant; no observer asked, no observer cap.
    #[test]
    fn a_lab_bus_lays_the_node_ring_and_an_observer_ring() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (keys, live) = (root.join("run/fabric"), root.join("sock/fabric"));
        std::fs::create_dir_all(root.join("run")).unwrap();
        std::fs::create_dir_all(root.join("sock")).unwrap();
        let aterm = Path::new("/b/aterm");
        assert!(aterm_link::person::is_person_id("a-lab-observer"));
        let laid = lay(
            aterm,
            &keys,
            &live,
            Some("a-lab-observer"),
            &mut fake_mint(None),
        )
        .unwrap();
        assert_eq!(
            grants_of(&laid.cap),
            aterm_link::enable::node_grants(FLEET, &laid.node)
        );
        let observer = laid.observer.clone().expect("an observer cap");
        assert_eq!(observer, keys.join("observer.cap"));
        assert_eq!(
            grants_of(&observer),
            aterm_link::enable::person_grants(FLEET, "a-lab-observer")
        );
        assert!(
            laid.node.len() == 18
                && laid.node.starts_with("n-")
                && laid.node[2..].bytes().all(|b| b.is_ascii_hexdigit()),
            "{}",
            laid.node
        );
        assert_eq!(
            std::fs::read_to_string(laid.state.join("node")).unwrap(),
            format!("{}\n", laid.node)
        );
        assert_eq!(std::fs::read_link(&laid.program).unwrap(), aterm);
        assert_eq!(std::fs::read(&laid.secret).unwrap().len(), 32);
        for d in [&keys, &live, &laid.state] {
            assert_eq!(mode(d), 0o700, "{}", d.display());
        }
        for f in [&laid.secret, &laid.cap, &observer, &laid.state.join("node")] {
            assert_eq!(mode(f), 0o600, "{}", f.display());
        }
        let command = laid.command();
        for word in &command {
            assert_eq!(aterm_link::enable::argv_safe(word), Ok(()), "{word}");
        }
        let line = command.join(" ");
        for want in [
            format!("--broker {}", live.join("b.sock").display()),
            format!("--cap-file {}", keys.join("node.cap").display()),
            format!("--state {}", live.join("state").display()),
            format!("--accept-from {}", laid.node),
        ] {
            assert!(line.contains(&want), "{want}: {line}");
        }
        let doc: aterm_toml::Value = aterm_toml::from_str(&laid.table).unwrap();
        assert_eq!(
            doc.get("fabric")
                .and_then(|t| t.get("command"))
                .and_then(aterm_toml::Value::as_str),
            Some(line.as_str())
        );
        assert_eq!(
            laid.broker_argv(),
            [
                keys.join("aterm").display().to_string(),
                "link".into(),
                "broker".into(),
                live.join("b.sock").display().to_string(),
                live.join("b.log").display().to_string(),
                "--secret-file".into(),
                keys.join("mint.secret").display().to_string(),
            ]
        );
        let back =
            aterm_link::enable::Rendezvous::parse(&laid.rendezvous().render()).expect("parses");
        assert_eq!(back, laid.rendezvous());
        assert_eq!(back.command, command);
        assert_eq!(
            (back.fleet.as_str(), back.node.as_str()),
            (FLEET, laid.node.as_str())
        );

        let failed = lay(
            aterm,
            &root.join("run2/fabric"),
            &root.join("sock2/fabric"),
            Some("a-lab-observer"),
            &mut fake_mint(Some(3)),
        )
        .expect_err("the third grant is refused");
        let third = &aterm_link::enable::node_grants(FLEET, "n-0")[2];
        assert!(
            failed.contains(&format!("aterm link mint {third}: ")) && failed.contains("QZP6WORD1"),
            "{failed}"
        );
        assert!(!root.join("run2/fabric/node.cap").exists());
        assert!(!root.join("run2/fabric/observer.cap").exists());
        let failed_observer = lay(
            aterm,
            &root.join("run3/fabric"),
            &root.join("sock3/fabric"),
            Some("a-lab-observer"),
            &mut fake_mint(Some(9)),
        )
        .expect_err("the observer's first grant is refused");
        assert!(
            failed_observer.contains("a-lab-observer"),
            "{failed_observer}"
        );
        assert!(!root.join("run3/fabric/observer.cap").exists());

        let plain = lay(
            aterm,
            &root.join("run4/fabric"),
            &root.join("sock4/fabric"),
            None,
            &mut fake_mint(None),
        )
        .unwrap();
        assert_eq!(plain.observer, None);
        assert!(!root.join("run4/fabric/observer.cap").exists());
        assert!(
            lay(
                aterm,
                &root.join("run5/fabric"),
                &root.join("sock5/fabric"),
                Some("lab-observer"),
                &mut fake_mint(None),
            )
            .is_err()
        );
    }

    /// The listen wait returns once the socket stands, and a broker that exits first ends it
    /// with what the broker said.
    #[test]
    fn a_broker_that_exits_before_it_listens_ends_the_wait_with_what_it_said() {
        let tmp = aterm_tempfile::tempdir().unwrap();
        let sock = tmp.path().join("b.sock");
        let e = await_listening(&sock, LISTEN_HANG, || true, || " no QZP6WORD2\n".into())
            .expect_err("gone");
        assert_eq!(e, "the lab's broker never listened: no QZP6WORD2");
        std::fs::write(&sock, "").unwrap();
        assert_eq!(
            await_listening(&sock, LISTEN_HANG, || true, String::new),
            Ok(())
        );
    }
}
