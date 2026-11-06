// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `swarm e2e`: the membership checks of one-mesh E2 (`docs/DESIGN-alab-one-mesh-2026-11-05.md`
//! §11 wave A, "E2. End-to-end: membership") that no other lane stages, on real processes.
//!
//! Five private Macs share this Mac's loopback, each a world of its own ([`world::world_env`])
//! on the run's own membership log, so no Mac reaches the live lab: `d` and `t` run headless
//! aterm instances (a fleet listener each) and are in the vault; `s`, `x` and `y` are vault-only
//! Macs. `d` mints the fleet's secret, founds the vault (`aterm secrets create`, which makes it
//! the vault's admin until D1) and serves it; `t` joins the fleet at `d`, and `d`'s desk lets it
//! into the vault by a person's Allow and the phrase, bound to `t`'s fleet node key, so the
//! swarm roster each Mac derives from its log names `t` by a label that resolves to `t`'s node
//! in `d`'s fleet book.
//!
//! **Drives by name under the vault's lock (N1, N2).** With a writer holding `d`'s state lock
//! (the product's own `StateLock`, taken in this process, [`client::test_hold_state_lock`]):
//! `aterm ctl --host <t's label>` from `d` waits the writer out and reaches `t`; past the drive's
//! own deadline it is refused to try again, never "no Mac this machine's book records"; and `aterm
//! ctl survey --fleet --json` lists every Mac, the one whose writer holds its lock reading
//! `roster_unread=1`, and with no writer every Mac reads its roster. The lane's own control: under
//! the same writer the unwaited roster read (`client::test_unwaited_roster`, what every reader did
//! before N1 and N3) names no Mac.
//!
//! **Seatless (S1).** `s`, `x` and `y` ask to join at `d`'s server and `d`'s admin approves each
//! by code before the log's genesis, which lists each as a founder with a label and no node key
//! and wraps its recipient: a seatless recipient, as ALab's genesis listed m9. Then `s` proves
//! itself to `d`'s desk with nobody's Allow (`members::join::prove_first`, the proof its
//! subscriber's ask makes), from a host on the desk's test home network, and must take its seat
//! with the same keys and the founder's own label for an `Admit` and a `Wrap` per epoch; `x` is
//! seated by a person's Allow and the phrase, as a Mac off the LAN is. The ask itself cannot fire
//! in a lab: the subscriber asks only for the compiled ALab vault and an unmarked identity
//! (`client/seat_ask.rs` `asks_with`), and a debug build's identity is a lab identity (A12); S1's
//! own tests hold that gate. A person's Allow seats `x` under its genesis label too (f184): a
//! person vouched for the founder the genesis lists. Then `d` removes `t`, and the rotation that
//! follows carries `s` and `x` and drops `y`, which nothing seated: the carry is the current seats
//! (A1). `y` then reads itself out and its `secrets status` says it was dropped, with the way back
//! as a new Mac, where `t`'s says revoked: only a `Remove` revokes (f204).
//!
//! A Mac's vault server is `aterm secrets serve`, the aterm under test; every desk, joiner and
//! record read runs the product's code as linked into this lab's image.

use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_secrets::client::{self, Paths};
use aterm_secrets::commons::bootstrap::Device;
use aterm_secrets::members::{desk, join, joiner};
use aterm_secrets::membership_log::Kind;
use aterm_types::MutexExt as _;
use aterm_uds::spawnfd::Stdio;

use crate::fleet_world::{self as world, World};
use crate::pyjson::J;
use crate::{Fail, Res, procs, stop};

/// `swarm e2e`'s usage.
pub const USAGE: &str = "swarm e2e [--aterm PATH] [--keep]";

/// Hang detector for an event that must happen.
const HANG: Duration = Duration::from_secs(60);

/// The vault's name on its server.
const VAULT: &str = "e2elab";

/// How long a writer holds `d`'s lock while a drive or a survey must wait it out: long enough
/// that a read which did not wait would have met it, well inside the drive's own deadline.
const HOLD: Duration = Duration::from_secs(3);

/// How long a writer holds `d`'s lock past a drive's deadline ([`SHORT_DRIVE`]).
const HOLD_PAST: Duration = Duration::from_secs(8);

/// The deadline of the drive that must be refused to try again (`aterm ctl --timeout`).
const SHORT_DRIVE: &str = "2";

/// The lane's Macs: name, world slot, whether it runs an instance.
const MACS: [(&str, usize, bool); 5] = [
    ("d", 0, true),
    ("t", 1, true),
    ("s", 2, false),
    ("x", 3, false),
    ("y", 4, false),
];

/// The seatless Macs: each asks to join by code, and `d`'s admin approves it before the log's
/// genesis, which lists it as a founder with a label and no node key, its recipient wrapped.
const SEATLESS: [&str; 3] = ["s", "x", "y"];

/// The address a seatless Mac proves itself from: a host on the desk's test home network
/// (`HomeNet::test`, this member at `192.168.1.20/24`).
const LAN_PEER: [u8; 4] = [192, 168, 1, 17];

struct Opts {
    aterm: Option<String>,
    keep: bool,
}

fn parse(args: &[String]) -> Result<Opts, Fail> {
    let mut rem = args.to_vec();
    let aterm = crate::instance::take_value(&mut rem, "--aterm")?;
    let mut keep = false;
    for arg in rem {
        match arg.as_str() {
            "--keep" => keep = true,
            "-h" | "--help" => return Err(Fail::usage(USAGE)),
            other => {
                return Err(Fail::usage(format!(
                    "swarm e2e: unknown argument {other:?}\nusage: {USAGE}"
                )));
            }
        }
    }
    Ok(Opts { aterm, keep })
}

/// One check's verdict.
struct Check {
    pass: bool,
    name: &'static str,
    said: String,
}

struct Lab {
    world: World,
    t0: Instant,
    /// Each instance Mac's index in `world.insts`.
    insts: BTreeMap<&'static str, usize>,
    fleet_ports: BTreeMap<&'static str, u16>,
    vault_port: u16,
    signer: String,
    /// The vault server's process, killed by PID when the lane ends.
    kids: Vec<(i32, String)>,
    lifelines: Vec<aterm_uds::lifeline::Lifeline>,
}

impl Drop for Lab {
    fn drop(&mut self) {
        self.lifelines.clear();
        for (pid, started) in &self.kids {
            // The server first: a wrapper the signal meets before its lifeline's read returns
            // dies without killing it, and the server would outlive the lane.
            for kid in procs::descendants(*pid) {
                let _ = crate::sys::kill(kid, libc::SIGTERM);
            }
            if crate::sys::alive(*pid) && procs::start_time(*pid) == *started {
                let _ = crate::sys::kill(*pid, libc::SIGTERM);
            }
        }
        for (pid, started) in &self.kids {
            if !procs::wait_gone(*pid, Duration::from_secs(5))
                && procs::start_time(*pid) == *started
            {
                let _ = crate::sys::kill(*pid, libc::SIGKILL);
            }
        }
    }
}

fn slot(mac: &str) -> usize {
    MACS.iter()
        .find(|(name, ..)| *name == mac)
        .map_or_else(|| unreachable!("no Mac {mac}"), |(_, slot, _)| *slot)
}

fn one_line(text: &str) -> String {
    crate::util::one_line(text.trim(), 300)
}

impl Lab {
    fn say(&self, line: &str) {
        println!("{:>9.1}s  {line}", self.t0.elapsed().as_secs_f64());
    }

    fn root(&self, mac: &str) -> PathBuf {
        self.world.root_of(slot(mac))
    }

    fn home(&self, mac: &str) -> PathBuf {
        self.root(mac).join("home")
    }

    fn paths(&self, mac: &str) -> Paths {
        Paths::for_home(&self.home(mac))
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v1/{VAULT}", self.vault_port)
    }

    fn pinned(&self) -> String {
        format!("{}#{}", self.url(), self.signer)
    }

    fn aterm(&self, mac: &str, args: &[&str]) -> world::Out {
        self.world.aterm_in(&self.root(mac), args, None)
    }

    fn aterm_ok(&self, mac: &str, args: &[&str], stdin: Option<&str>) -> Result<String, Fail> {
        let out = self.world.aterm_in(&self.root(mac), args, stdin);
        if out.code == 0 {
            Ok(out.stdout)
        } else {
            Err(Fail::die(format!(
                "{mac}: aterm {} exited {}: {}",
                args.join(" "),
                out.code,
                one_line(&format!("{} {}", out.stdout, out.stderr))
            )))
        }
    }

    fn inst(&self, mac: &str) -> &world::Inst {
        &self.world.insts[self.insts[mac]]
    }

    /// The boot session's sid of `mac`'s instance.
    fn boot_sid(&self, mac: &str) -> Result<String, Fail> {
        let body = self
            .world
            .ctl_ok(self.inst(mac), &["sessions"])
            .map_err(Fail::die)?;
        body.lines()
            .find(|l| l.starts_with(|c: char| c.is_ascii_digit()))
            .and_then(|row| row.split_whitespace().nth(1))
            .map(str::to_owned)
            .ok_or_else(|| Fail::die(format!("{mac}: no boot session: {}", one_line(&body))))
    }

    fn start_instance(&mut self, mac: &'static str) -> Result<(), Fail> {
        let inst = self
            .world
            .start(slot(mac), self.fleet_ports[mac])
            .map_err(Fail::die)?;
        self.world.insts.push(inst);
        self.insts.insert(mac, self.world.insts.len() - 1);
        // Its boot session, which the world's status reads (`fleet_word`) and the drives name.
        let sid = self.boot_sid(mac)?;
        if let Some(inst) = self.world.insts.last_mut() {
            inst.sessions.push(world::Sess {
                sid,
                kind: world::Kind::Shell,
                n: 0,
            });
        }
        Ok(())
    }

    /// `aterm secrets serve` in `d`'s world, tied to this process by a lifeline.
    fn start_server(&mut self) -> Result<(), Fail> {
        let root = self.root("d");
        let listen = format!("127.0.0.1:{}", self.vault_port);
        let out = crate::util::create_stdio_file(&root.join("vault-server.log"))
            .map_err(|e| Fail::die(format!("d: its server's log: {e}")))?;
        let err = out.try_clone().map_err(|e| Fail::die(e.to_string()))?;
        let (lifeline, far) = aterm_uds::lifeline::Lifeline::open(&root)
            .map_err(|e| Fail::die(format!("d: a lifeline: {e}")))?;
        let mut cmd = aterm_uds::spawnfd::command("/bin/sh");
        cmd.arg("-c")
            .arg("\"$@\" </dev/null & kid=$!; read -r _; kill \"$kid\" 2>/dev/null; wait \"$kid\"")
            .arg("lab-d-vault-server")
            .arg(&self.world.aterm)
            .args([
                "secrets",
                "serve",
                "--listen",
                &listen,
                "--no-advertise",
                "--no-keep-awake",
            ])
            .env_clear()
            .envs(self.world.env_of(&root))
            .current_dir(root.join("home"))
            .setsid(true)
            .stdin(Stdio::from(far))
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        let pid = procs::spawn_kept(&mut cmd)
            .map_err(|e| Fail::die(format!("d: cannot start its vault server: {e}")))?;
        self.lifelines.push(lifeline);
        self.kids.push((pid, procs::start_time(pid)));
        let addr: std::net::SocketAddr = listen.parse().map_err(|_| Fail::die("an address"))?;
        let end = Instant::now() + HANG;
        while std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_err() {
            if Instant::now() >= end {
                return Err(Fail::die(format!(
                    "d's vault server did not listen on {listen} within {HANG:?}: {}",
                    world::tail(&root.join("vault-server.log"), 8)
                )));
            }
            stop::sleep(Duration::from_millis(100))?;
        }
        Ok(())
    }

    /// Wait (the hang detector) until `ready` answers, naming `what` when it never does.
    fn until<T>(&self, what: &str, mut ready: impl FnMut() -> Option<T>) -> Result<T, Fail> {
        let end = Instant::now() + HANG;
        loop {
            if let Some(v) = ready() {
                return Ok(v);
            }
            if Instant::now() >= end {
                return Err(Fail::die(format!("{what}: not within {HANG:?}")));
            }
            stop::sleep(Duration::from_millis(250))?;
        }
    }

    /// `mac`'s fleet node identity, which its instance made.
    fn node(&self, mac: &str) -> Result<aterm_net::fleet::NodeIdentity, Fail> {
        aterm_net::fleet_member::read_node_identity(&self.root(mac).join("state"))
            .map_err(|e| Fail::die(format!("{mac}'s node identity: {e}")))?
            .ok_or_else(|| Fail::die(format!("{mac} holds no node identity")))
    }

    /// `mac`'s age recipient, as `age1…`.
    fn recipient(&self, mac: &str) -> Result<String, Fail> {
        client::machine_identity(&self.paths(mac))
            .map(|id| id.recipient().to_bech32())
            .map_err(|e| Fail::die(format!("{mac}'s identity: {e}")))
    }

    /// The label `by`'s swarm roster gives the Mac whose node key is `key_hex`.
    fn roster_label(&self, by: &str, key_hex: &str) -> Option<String> {
        client::swarm_roster_until(&self.paths(by), Instant::now() + HANG)
            .ok()
            .flatten()?
            .members
            .into_iter()
            .find(|m| {
                m.node_key
                    .as_deref()
                    .is_some_and(|k| k.eq_ignore_ascii_case(key_hex))
            })
            .map(|m| m.label)
    }

    /// What `mac`'s roster and log say, for a wait that failed: each roster row's label and
    /// node key prefix, and each seat's label and node key prefix.
    fn diag(&self, mac: &str) -> String {
        let paths = self.paths(mac);
        let roster = match client::swarm_roster_until(&paths, Instant::now() + HANG) {
            Ok(Some(r)) => r
                .members
                .iter()
                .map(|m| {
                    format!(
                        "{} key={} id={}",
                        m.label,
                        m.node_key.as_deref().map_or("-", |k| &k[..k.len().min(8)]),
                        m.node_id.as_deref().map_or("-", |k| &k[..k.len().min(8)])
                    )
                })
                .collect::<Vec<_>>()
                .join(", "),
            Ok(None) => "none".to_owned(),
            Err(e) => format!("unread: {e}"),
        };
        let seats = client::load_records(&paths).map_or_else(
            |e| format!("unread: {e}"),
            |store| {
                store.log().map_or_else(
                    || "no log".to_owned(),
                    |log| {
                        log.membership()
                            .members()
                            .values()
                            .map(|m| {
                                format!(
                                    "{} key={}",
                                    m.label,
                                    m.node_key.map_or_else(
                                        || "-".to_owned(),
                                        |k| aterm_codec::hex::encode_lower(&k[..4])
                                    )
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                )
            },
        );
        let status = self.aterm(mac, &["secrets", "status"]);
        format!(
            "{mac}'s roster: [{roster}]; its seats: [{seats}]; secrets status: {}",
            one_line(&format!("{} {}", status.stdout, status.stderr))
        )
    }

    /// The kinds of the records `d`'s log holds now, in order.
    fn log_kinds(&self, mac: &str) -> Vec<(String, Kind)> {
        client::load_records(&self.paths(mac))
            .ok()
            .and_then(|store| {
                store.log().map(|log| {
                    log.records()
                        .map(|r| (format!("{:?}", r.id()), r.kind()))
                        .collect()
                })
            })
            .unwrap_or_default()
    }

    /// The label `mac`'s log's genesis gives the founder whose recipient is `recipient` and
    /// whom no seat binds yet (an unproved founder), if it lists one.
    fn founder_label(&self, mac: &str, recipient: &str) -> Option<String> {
        let store = client::load_records(&self.paths(mac)).ok()?;
        let log = store.log()?;
        log.membership()
            .unproved()
            .iter()
            .find(|f| f.recipient.to_bech32() == recipient)
            .map(|f| f.label.clone())
    }

    /// The label of the seat `mac`'s own log binds to `recipient`, if one does.
    fn seat_label(&self, mac: &str, recipient: &str) -> Option<String> {
        let store = client::load_records(&self.paths(mac)).ok()?;
        let log = store.log()?;
        log.membership()
            .members()
            .values()
            .find(|m| m.binding.recipient().to_bech32() == recipient)
            .map(|m| m.label.clone())
    }
}

/// How a Mac comes in at a member's desk.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Way {
    /// A person's Allow on the desk and the phrase, the request carrying this node key (`[0; 32]`:
    /// none), from this Mac's loopback.
    Allow([u8; 32]),
    /// The Mac's own proof with nobody's Allow: its writer key and its fleet node key sign the
    /// desk's challenge and it answers the possession challenge (`members::join::prove_first`,
    /// the proof the subscriber's seat ask makes), from a host on the desk's home LAN
    /// ([`LAN_PEER`], [`on_test_lan`]).
    Prove,
}

/// `by`'s desk on the test home network (`desk::Local::with_automatic_lan`), so a proof from
/// [`LAN_PEER`] meets the home-LAN rule as one from another Mac on ALab's LAN does. A lab vault on
/// loopback has no home LAN of its own: the rule reads the place this Mac last read.
#[cfg(debug_assertions)]
fn on_test_lan(local: desk::Local) -> desk::Local {
    local.with_automatic_lan(Vec::<String>::new())
}

#[cfg(not(debug_assertions))]
fn on_test_lan(local: desk::Local) -> desk::Local {
    local
}

/// `by` lets the Mac whose home is `joiner_home` in, `way`. The joiner's side is the product's
/// own (`members::join`'s `prove_first`, `ask_all` and `run_phrase`, which writes the bundle it
/// is handed, or takes only its seat's records where it holds the vault already, and follows on);
/// the desk is `by`'s, served in this process ([`session`]). `by` then posts its records to the
/// servers it reaches, as its window does. The name the joiner came in under, and how many
/// servers took `by`'s records.
fn admit(by: &Paths, joiner_home: &Path, name: &str, way: Way) -> Result<String, String> {
    let joiner = Paths::for_home(joiner_home);
    client::machine_identity(&joiner).map_err(|e| e.to_string())?;
    let device = Device::open(&joiner)
        .map_err(|e| format!("the joiner's device: {e}"))?
        .ok_or("the joiner holds no identity")?;
    // A proof signs with the joiner's fleet node key, made in its world's state if it has none.
    let node = match way {
        Way::Allow(_) => None,
        Way::Prove => {
            let state = joiner_home
                .parent()
                .map(|root| root.join("state"))
                .ok_or("the joiner's home has no world")?;
            Some(aterm_net::fleet_member::node_identity(&state).map_err(|e| e.to_string())?)
        }
    };
    let store = client::load_records(&joiner).map_err(|e| e.to_string())?;
    let joining = joiner::Joining {
        device: &device,
        records_bound: client::records_bound(&joiner, &store).map_err(|e| e.to_string())?,
        node: match (way, &node) {
            (Way::Allow(key), _) => key,
            (Way::Prove, Some(node)) => *node.key().bytes(),
            (Way::Prove, None) => return Err("no node key to prove".to_owned()),
        },
        name: name.to_owned(),
        model: "Mac16,10".to_owned(),
    };
    let local = desk_of(by)?;
    let (local, from) = match way {
        Way::Allow(_) => (local, std::net::IpAddr::from([127, 0, 0, 1])),
        Way::Prove => (on_test_lan(local), std::net::IpAddr::from(LAN_PEER)),
    };
    let desk = std::sync::Mutex::new(desk::Desk::new(local));
    let hand = |handing: &desk::Handing<'_>| client::hand_over(by, handing);
    let book = |proving: &desk::Proving<'_>| {
        aterm_agent::vault_join::book_word_read(
            None,
            &proving.node,
            Some(from),
            aterm_time::unix_millis().unwrap_or(0),
        )
    };
    let decide = |proving: &desk::Proving<'_>| client::prove_at(by, proving, book(proving));
    let refuses =
        |proving: &desk::Proving<'_>| client::refuses_before_possession(by, proving, book(proving));
    let proof = desk::Proof {
        decide: &decide,
        refuses: &refuses,
        fleet: None,
    };
    let mut serve = |stream: &mut UnixStream| {
        desk::serve_with(&desk, stream, from, &hand, Some(proof))
            .map(drop)
            .map_err(|e| e.to_string())
    };
    let ats = [AT.to_owned()];
    let with_desk = |why: String, refused: &[String]| {
        if refused.is_empty() {
            why
        } else {
            format!("{why} (the desk: {})", refused.join("; "))
        }
    };
    let (joined, how) = if let (Way::Prove, Some(node)) = (way, &node) {
        let sign = |message: &[u8]| node.sign_record(desk::PROVE_RECORD, message);
        let (proved, refused) = session(&mut serve, &|dial| {
            join::prove_first(&joiner, dial, &joining, &sign, &ats).map_err(|missed| {
                missed
                    .refused
                    .into_iter()
                    .map(|(_, why)| why)
                    .collect::<Vec<_>>()
                    .join("; ")
            })
        })?;
        (
            proved.map_err(|why| with_desk(why, &refused))?,
            "its own proof",
        )
    } else {
        let ((heard, missed), _) =
            session(&mut serve, &|dial| join::ask_all(dial, &joining, &ats))?;
        if heard.is_empty() {
            return Err(format!("the desk did not answer the ask: {missed:?}"));
        }
        let phrase = {
            let mut desk = desk.lock_or_recover();
            let now = aterm_time::Instant::now();
            let row = desk
                .rows(now)
                .first()
                .map(|row| row.id)
                .ok_or("the desk shows no request")?;
            let phrase = desk.allow(row, now).map_err(|e| e.to_string())?;
            desk.words(phrase, now)
                .ok_or("the phrase ran out before it was read")?
                .join(" ")
        };
        let phrase = aterm_secrets::phrase::Phrase::parse(&phrase).map_err(|e| e.to_string())?;
        let (ran, refused) = session(&mut serve, &|dial| {
            let (heard, missed) = join::ask_all(dial, &joining, &ats);
            if heard.is_empty() {
                return Err(format!("the desk did not answer the ask: {missed:?}"));
            }
            join::run_phrase(&joiner, dial, &joining, &heard, &phrase, None, &mut |_| {})
                .map_err(|e| e.to_string())
        })?;
        (
            ran.map_err(|why| with_desk(why, &refused))?,
            "Allow and the phrase",
        )
    };
    let posted = client::post_records_to_servers(by).unwrap_or(0);
    Ok(format!(
        "{} in by {how} ({posted} servers took the admitter's records)",
        joined.name
    ))
}

/// The address the joiner dials the desk at: every connection its dial makes goes to the desk
/// this process serves ([`session`]).
const AT: &str = "desk";

/// One session of the joiner with a desk, both in this process: `joining` runs on a thread of
/// its own with a dial whose every connection `serve` takes here, one at a time, until `joining`
/// returns. Each end reads under the hang detector. What `joining` returned, and every refusal
/// the desk ended a connection with.
fn session<R: Send>(
    serve: &mut dyn FnMut(&mut UnixStream) -> Result<(), String>,
    joining: &(dyn Fn(&join::Dial<'_>) -> R + Sync),
) -> Result<(R, Vec<String>), String> {
    let (to_desk, at_desk) = std::sync::mpsc::channel::<UnixStream>();
    std::thread::scope(|scope| {
        let joined = scope.spawn(move || {
            let dial = move |_: &str| -> aterm_secrets::Result<Box<dyn join::Stream>> {
                let (ours, theirs) = UnixStream::pair()
                    .map_err(|e| aterm_secrets::Error::new(format!("a socket pair: {e}")))?;
                for end in [&ours, &theirs] {
                    end.set_read_timeout(Some(HANG))
                        .map_err(|e| aterm_secrets::Error::new(e.to_string()))?;
                }
                to_desk
                    .send(ours)
                    .map_err(|_| aterm_secrets::Error::new("the desk stopped"))?;
                Ok(Box::new(theirs))
            };
            joining(&dial)
        });
        let mut refused = Vec::new();
        for mut stream in at_desk {
            if let Err(why) = serve(&mut stream) {
                refused.push(why);
            }
        }
        joined
            .join()
            .map(|r| (r, refused))
            .map_err(|_| "the joiner panicked".to_owned())
    })
}

/// `by`'s desk, one that can allow, waited for under the hang detector as its window follows it
/// (a read during a vault pass finds none for a moment).
fn desk_of(by: &Paths) -> Result<desk::Local, String> {
    let start = Instant::now();
    loop {
        let why = match desk::Local::of(by) {
            Ok(Some(local)) if local.can_allow() => return Ok(local),
            Ok(Some(_)) => {
                "neither the membership log seats this Mac nor a Commons roster binds it".to_owned()
            }
            Ok(None) => "this Mac is no member".to_owned(),
            Err(e) => format!("this Mac's membership: {e}"),
        };
        if start.elapsed() >= HANG {
            return Err(format!("{why} (no desk that can allow came in {HANG:?})"));
        }
        stop::sleep(Duration::from_millis(250)).map_err(|_| "interrupted".to_owned())?;
    }
}

/// How many Macs the read every roster reader made before N1 and N3 names (N3's seam for it,
/// [`client::test_unwaited_roster`]): under a writer it answers no roster. The lane's negative
/// control; a release lab image has no seam and no lock to read it under ([`under_lock`]).
#[cfg(debug_assertions)]
fn unwaited_roster(paths: &Paths) -> Result<Option<usize>, String> {
    client::test_unwaited_roster(paths)
        .map(|r| r.map(|r| r.members.len()))
        .map_err(|e| e.to_string())
}

#[cfg(not(debug_assertions))]
fn unwaited_roster(_: &Paths) -> Result<Option<usize>, String> {
    Err("a release lab image has no unwaited roster read".to_owned())
}

/// Hold `mac`'s vault state lock, as a writer mid-apply does, for `hold` from the moment it is
/// taken; `taken` hears once it is held. The lock is the product's own `StateLock`.
#[cfg(debug_assertions)]
fn hold_lock(paths: Paths, hold: Duration, taken: std::sync::mpsc::Sender<Result<(), String>>) {
    let held = client::test_hold_state_lock(&paths);
    let _ = taken.send(held.as_ref().map(drop).map_err(ToString::to_string));
    if held.is_ok() {
        std::thread::sleep(hold);
    }
    drop(held);
}

/// Run `act` while a writer holds `mac`'s state lock for `hold`, started once the lock is held.
/// What `act` returned and how long it took. A lock the writer could not take stops the lane: no
/// check may say a writer held it.
fn under_lock<T>(
    lab: &Lab,
    mac: &str,
    hold: Duration,
    act: impl FnOnce() -> T,
) -> Result<(T, Duration), Fail> {
    #[cfg(debug_assertions)]
    {
        let (taken, held) = std::sync::mpsc::channel();
        let paths = lab.paths(mac);
        // Not a scoped thread: a writer still waiting on a lock held elsewhere must not hold the
        // lane past its hang detector, so on a timeout it is left to the process's end.
        let writer = std::thread::spawn(move || hold_lock(paths, hold, taken));
        match held.recv_timeout(HANG) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = writer.join();
                return Err(Fail::die(format!("{mac}'s state lock was not taken: {e}")));
            }
            Err(_) => {
                return Err(Fail::die(format!(
                    "{mac}'s state lock was not taken within {HANG:?}"
                )));
            }
        }
        let start = Instant::now();
        let out = act();
        let took = start.elapsed();
        let _ = writer.join();
        Ok((out, took))
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (lab, hold, act);
        Err(Fail::die(format!(
            "{mac}'s state lock: a release lab image holds no lock by the test seam"
        )))
    }
}

/// Whether a survey row (`roster_unread`, `label`) lists its Mac while a writer holds that Mac's
/// lock: its roster busy (`roster_unread=1`), or a snapshot its listener read within the minute
/// before, which names its label. Either way the Mac is listed, never missing (N2).
fn listed_under_lock(row: Option<&(String, String)>) -> bool {
    row.is_some_and(|(unread, label)| unread == "1" || (unread == "0" && label != "-"))
}

/// `aterm ctl survey --fleet --json` from `mac`: for each Mac whose instance answered (by PID),
/// its `roster_unread` word and its `label`.
fn survey(lab: &Lab, from: &str) -> (BTreeMap<String, (String, String)>, String) {
    let out = lab.aterm(from, &["ctl", "survey", "--fleet", "--json"]);
    let mut answered = BTreeMap::new();
    if let Ok(J::Arr(hosts)) = J::parse(out.stdout.trim()) {
        for host in hosts {
            let Some(instance) = host.get("instance") else {
                continue;
            };
            let field = |k: &str| instance.get(k).and_then(J::as_str).unwrap_or("").to_owned();
            let pid = field("pid");
            for (mac, i) in &lab.insts {
                if lab.world.insts[*i].pid.to_string() == pid {
                    answered.insert((*mac).to_owned(), (field("roster_unread"), field("label")));
                }
            }
        }
    }
    let raw = if answered.is_empty() {
        one_line(&format!("exit {}: {} {}", out.code, out.stdout, out.stderr))
    } else {
        String::new()
    };
    (answered, raw)
}

/// Print `check` as it is judged, and keep it.
fn push(checks: &mut Vec<Check>, check: Check) {
    println!(
        "- [{}] {}: {}",
        if check.pass { "PASS" } else { "FAIL" },
        check.name,
        check.said
    );
    checks.push(check);
}

/// What a refused drive must never say: the words of the defect N1 fixed.
const NO_MAC: &str = "no Mac this machine's book records";

/// The run: bring-up, the drives and surveys under the lock, then the seatless Macs.
#[allow(clippy::too_many_lines)]
fn run(lab: &mut Lab) -> Result<Vec<Check>, Fail> {
    let mut checks = Vec::new();
    // The fleet: d mints, t joins at d, both allow drive.
    lab.start_instance("d")?;
    lab.start_instance("t")?;
    let d_port = lab.fleet_ports["d"].to_string();
    lab.aterm_ok(
        "d",
        &[
            "fleet",
            "init",
            "--host",
            "127.0.0.1",
            "--port",
            &d_port,
            "--allow",
            "content",
        ],
        None,
    )?;
    lab.world
        .await_listening(lab.inst("d"))
        .map_err(Fail::die)?;
    let code = crate::util::read_regular_text(&lab.root("d").join("state/fleet-join-code"))
        .map_err(|e| Fail::die(format!("the fleet's join code: {e}")))?;
    let t_port = lab.fleet_ports["t"].to_string();
    let at = format!("127.0.0.1:{d_port}");
    lab.aterm_ok(
        "t",
        &[
            "fleet",
            "join",
            "--host",
            "127.0.0.1",
            "--port",
            &t_port,
            "--allow",
            "content",
            "--at",
            &at,
        ],
        Some(code.as_str()),
    )?;
    lab.world
        .await_listening(lab.inst("t"))
        .map_err(Fail::die)?;
    for mac in ["d", "t"] {
        lab.aterm_ok(mac, &["fleet", "allow", "drive", "--all"], None)?;
    }
    let (d_node, t_node) = (lab.node("d")?, lab.node("t")?);
    let t_key = t_node.key().hex();
    lab.until("d's fleet book binds t's node key", || {
        let keys = aterm_net::fleet_member::Keyring::read(&lab.root("d").join("state"));
        (keys.bound(t_node.node()) == Some(t_node.key())).then_some(())
    })?;
    lab.say(&format!(
        "fleet formed: d ({}) minted, t ({}) joined at d; both allow drive --all",
        d_node.node(),
        t_node.node()
    ));

    // The vault: d founds and serves it; t comes in by d's desk, bound to its node key.
    lab.start_server()?;
    let out = lab.aterm_ok(
        "d",
        &["secrets", "create", &lab.url(), "--name", "d", "--no-agent"],
        None,
    )?;
    lab.signer = out
        .lines()
        .find_map(|l| l.strip_prefix("signer: "))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_owned)
        .ok_or_else(|| Fail::die(format!("d's create printed no signer: {}", one_line(&out))))?;
    // Seatless Macs: s, x and y ask to join at d's server and d's admin approves each by code,
    // before the log's genesis, which then lists each as a founder with a label and no node key
    // and wraps its recipient, as ALab's genesis listed m9 (one-mesh S1, Step 0).
    for mac in SEATLESS {
        world::make_world(&lab.root(mac))
            .map_err(|e| Fail::die(format!("{mac}: its world: {e}")))?;
        lab.aterm_ok(
            mac,
            &[
                "secrets",
                "join",
                &lab.pinned(),
                "--name",
                mac,
                "--no-agent",
                "--no-wait",
            ],
            None,
        )?;
    }
    let mut recs = BTreeMap::new();
    for mac in SEATLESS {
        let rec = lab.recipient(mac)?;
        // `approve --all` asks for a yes at a terminal; one key and its name does not.
        lab.aterm_ok("d", &["secrets", "approve", &rec, "--name", mac], None)?;
        recs.insert(mac, rec);
    }
    // The run's own membership log: `secrets create` founds the vault and Commons, and its
    // genesis is seeded as `lab mesh churn` seeds one (D1's `fleet init` does both).
    let d_paths = lab.paths("d");
    client::seed_vault_genesis(&d_paths, &d_paths.dir.join("e2e.genesis"))
        .map_err(|e| Fail::die(format!("d seeds the log's genesis: {e}")))?;
    let posted = client::post_records_to_servers(&d_paths).unwrap_or(0);
    lab.say(&format!(
        "d founded vault {VAULT}, approved s, x and y by code, seeded its log's genesis ({posted} \
         servers took it), and serves it on {}",
        lab.url()
    ));
    let said = admit(
        &lab.paths("d"),
        &lab.home("t"),
        "t",
        Way::Allow(*t_node.key().bytes()),
    )
    .map_err(|e| Fail::die(format!("d lets t in: {e}")))?;
    lab.say(&format!("t: {said}"));
    let t_label = lab
        .until("d's roster names t's node key", || {
            lab.roster_label("d", &t_key)
        })
        .map_err(|f| Fail::die(format!("{}; {}", f.msg.unwrap_or_default(), lab.diag("d"))))?;
    lab.until("t reads itself seated", || {
        let _ = lab.aterm("t", &["secrets", "sync"]);
        (client::seat(&lab.paths("t")) == client::Seat::Seated).then_some(())
    })?;
    lab.until("t's roster names t", || lab.roster_label("t", &t_key))?;
    let t_sid = lab.boot_sid("t")?;
    let at_t = format!("@{t_sid}");
    lab.say(&format!(
        "d's roster names t {t_label}; t's boot session {t_sid}"
    ));

    // D0: the drive by t's label, no writer.
    let drive = |timeout: Option<&str>| {
        let mut args = vec!["ctl"];
        if let Some(t) = timeout {
            args.extend(["--timeout", t]);
        }
        args.extend(["--host", t_label.as_str(), at_t.as_str(), "status"]);
        lab.aterm("d", &args)
    };
    let start = Instant::now();
    let free = drive(None);
    let free_ms = start.elapsed().as_millis();
    push(
        &mut checks,
        Check {
            pass: free.code == 0 && free.stdout.starts_with("OK"),
            name: "drive-by-label",
            said: format!(
                "with no writer, `ctl --host {t_label} @{t_sid} status` from d exited {} in {free_ms} ms{}",
                free.code,
                if free.code == 0 {
                    String::new()
                } else {
                    format!(": {}", one_line(&free.stderr))
                }
            ),
        },
    );

    // The lane's control: under a writer, the unwaited roster read names no Mac.
    let (unwaited, _) = under_lock(lab, "d", Duration::from_millis(500), || {
        (
            unwaited_roster(&lab.paths("d")),
            client::swarm_roster_until(&lab.paths("d"), Instant::now())
                .map(|r| r.map(|r| r.members.len())),
        )
    })?;
    push(
        &mut checks,
        Check {
            pass: matches!(unwaited.0, Ok(None)) && unwaited.1.is_err(),
            name: "control-unwaited-read",
            said: format!(
                "under d's writer the unwaited roster read (before N1) gives {:?} (no Mac named), the \
             bounded read at an expired deadline {}",
                unwaited.0,
                match &unwaited.1 {
                    Ok(n) => format!("{n:?}"),
                    Err(e) => format!("refuses: {}", one_line(&e.to_string())),
                }
            ),
        },
    );

    // D1: a writer holds d's lock for HOLD; the drive waits it out and reaches t.
    let (waited, took) = under_lock(lab, "d", HOLD, || drive(None))?;
    push(
        &mut checks,
        Check {
            pass: waited.code == 0 && waited.stdout.starts_with("OK"),
            name: "drive-under-lock",
            said: format!(
                "a writer held d's state lock {:.0}s; the drive by {t_label} exited {} after {} ms{}",
                HOLD.as_secs_f64(),
                waited.code,
                took.as_millis(),
                if waited.code == 0 {
                    String::new()
                } else {
                    format!(": {}", one_line(&waited.stderr))
                }
            ),
        },
    );

    // D2: a writer holds d's lock past the drive's deadline: refused to try again, no "no Mac".
    let (refused, took) = under_lock(lab, "d", HOLD_PAST, || drive(Some(SHORT_DRIVE)))?;
    let words = format!("{} {}", refused.stdout, refused.stderr);
    push(
        &mut checks,
        Check {
            pass: refused.code != 0 && words.contains("try again") && !words.contains(NO_MAC),
            name: "drive-past-deadline",
            said: format!(
                "a writer held d's lock {:.0}s; the drive with --timeout {SHORT_DRIVE} exited {} after \
             {} ms: {}",
                HOLD_PAST.as_secs_f64(),
                refused.code,
                took.as_millis(),
                one_line(&words)
            ),
        },
    );

    // D3: a writer holds the target's lock: the drive reaches t or is refused to try again.
    let (target, took) = under_lock(lab, "t", HOLD, || drive(None))?;
    let words = format!("{} {}", target.stdout, target.stderr);
    push(
        &mut checks,
        Check {
            pass: (target.code == 0 && target.stdout.starts_with("OK"))
                || (words.contains("try again") && !words.contains(NO_MAC)),
            name: "drive-target-locked",
            said: format!(
                "a writer held t's lock {:.0}s; the drive by {t_label} exited {} after {} ms{}",
                HOLD.as_secs_f64(),
                target.code,
                took.as_millis(),
                if target.code == 0 {
                    String::new()
                } else {
                    format!(": {}", one_line(&words))
                }
            ),
        },
    );

    // S-a: survey from d while t's writer holds t's lock: t answers, its roster busy.
    let ((busy_t, raw), took) = under_lock(lab, "t", HOLD, || survey(lab, "d"))?;
    let t_row = busy_t.get("t").cloned();
    push(
        &mut checks,
        Check {
            pass: busy_t.contains_key("d") && listed_under_lock(t_row.as_ref()),
            name: "survey-target-locked",
            said: format!(
                "a writer held t's lock; d's survey ({} ms) answered {:?} {raw}",
                took.as_millis(),
                busy_t
            ),
        },
    );

    // S-b: survey from d while d's own writer holds d's lock: both answer, d's roster busy.
    let ((busy_d, raw), took) = under_lock(lab, "d", HOLD, || survey(lab, "d"))?;
    push(
        &mut checks,
        Check {
            pass: busy_d.contains_key("t") && listed_under_lock(busy_d.get("d")),
            name: "survey-own-locked",
            said: format!(
                "a writer held d's lock; d's survey ({} ms) answered {:?} {raw}",
                took.as_millis(),
                busy_d
            ),
        },
    );

    // S-c: no writer: every Mac answers with its label read.
    let (idle, mut raw) = survey(lab, "d");
    if idle.values().any(|(unread, _)| unread != "0") {
        // Why a roster read failed with no writer: a drive by a label from that Mac's own world
        // quotes its roster read's error when the read fails.
        let probe = lab.aterm("t", &["ctl", "--host", "m99", &at_t, "status"]);
        let here = client::swarm_roster_until(
            &lab.paths("t"),
            Instant::now() + Duration::from_millis(250),
        )
        .map(|r| r.map(|r| r.members.len()));
        raw = format!(
            "{raw}; this lab's own bounded read of t's roster gives {:?}; a drive by m99 from t's \
             own world exited {}: {}; {}",
            here.as_ref().map_err(ToString::to_string),
            probe.code,
            one_line(&format!("{} {}", probe.stdout, probe.stderr)),
            lab.diag("t")
        );
    }
    push(
        &mut checks,
        Check {
            pass: idle.len() == 2 && idle.values().all(|(unread, _)| unread == "0"),
            name: "survey-idle",
            said: format!("with no writer, d's survey answered {idle:?} {raw}"),
        },
    );

    // Seatless (S1): each founder reads itself Seatless, wrapped with no seat.
    let mut last = String::new();
    let made = lab.until("s, x and y read themselves seatless", || {
        let mut seats = Vec::new();
        for mac in SEATLESS {
            let sync = lab.aterm(mac, &["secrets", "sync"]);
            let seat = client::seat(&lab.paths(mac));
            last = format!(
                "{mac} sync exit {}: {}; {mac} reads {seat:?}",
                sync.code,
                one_line(&format!("{} {}", sync.stdout, sync.stderr)),
            );
            seats.push(seat);
        }
        seats
            .iter()
            .all(|seat| *seat == client::Seat::Seatless)
            .then_some(())
    });
    let founders: BTreeMap<&str, Option<String>> = SEATLESS
        .iter()
        .map(|mac| (*mac, lab.founder_label("d", &recs[mac])))
        .collect();
    let s_status = lab.aterm("s", &["members", "status"]);
    push(
        &mut checks,
        Check {
            pass: made.is_ok() && founders.values().all(Option::is_some),
            name: "seatless-made",
            said: format!(
                "d's approval by code before the genesis made s, x and y founders with no node key: \
             {}; their labels {founders:?}; `aterm members status` on s says {:?}; the \
             subscriber's own ask fires: {} (a lab identity asks nothing, S1)",
                if made.is_ok() {
                    "each reads Seatless".to_owned()
                } else {
                    format!("not all Seatless within {HANG:?}: last {last}")
                },
                one_line(&format!("{} {}", s_status.stdout, s_status.stderr)),
                client::seat_asks(&lab.paths("s")),
            ),
        },
    );
    if made.is_err() {
        return Ok(checks);
    }

    // s proves itself to d's desk from the home LAN, with nobody's Allow: the proof its
    // subscriber's ask makes. Same keys, the founder's own label, an Admit and a Wrap per epoch.
    let seat_by = |mac: &'static str, way: Way, name: &'static str| {
        let before = lab.log_kinds("d");
        let said = admit(&lab.paths("d"), &lab.home(mac), mac, way);
        let added: Vec<Kind> = lab
            .log_kinds("d")
            .iter()
            .filter(|(id, _)| !before.iter().any(|(b, _)| b == id))
            .map(|(_, kind)| *kind)
            .collect();
        let seated = lab
            .until(&format!("{mac} reads itself seated"), || {
                let _ = lab.aterm(mac, &["secrets", "sync"]);
                (client::seat(&lab.paths(mac)) == client::Seat::Seated).then_some(())
            })
            .is_ok();
        let same_rec = lab.recipient(mac).is_ok_and(|rec| rec == recs[mac]);
        let label = lab.seat_label("d", &recs[mac]);
        let pass = said.is_ok()
            && seated
            && same_rec
            && (2..=5).contains(&added.len())
            && added.iter().filter(|k| **k == Kind::Admit).count() == 1;
        let said = format!(
            "{}; {mac} seated: {seated}, same recipient: {same_rec}; it cost {} records {added:?}; \
             its seat's label {label:?}, its founder label {:?}",
            match &said {
                Ok(s) => s.clone(),
                Err(e) => format!("refused: {e}"),
            },
            added.len(),
            founders[mac],
        );
        (Check { pass, name, said }, label)
    };
    let (proved, s_label) = seat_by("s", Way::Prove, "seatless-seated-by-proof");
    push(&mut checks, proved);
    push(
        &mut checks,
        Check {
            pass: s_label.is_some() && s_label == founders["s"],
            name: "seatless-label-kept",
            said: format!(
                "s's seat label {s_label:?}, the label the genesis gave it {:?}",
                founders["s"]
            ),
        },
    );
    // x, off the LAN, is seated by a person's Allow and the phrase.
    let (allowed, x_label) = seat_by("x", Way::Allow([0; 32]), "seatless-seated-by-allow");
    push(&mut checks, allowed);
    push(
        &mut checks,
        Check {
            pass: x_label.is_some() && x_label == founders["x"],
            name: "seatless-label-kept-by-allow",
            said: format!(
                "a person's Allow seated the founder x under {x_label:?}; the genesis gave it {:?}",
                founders["x"]
            ),
        },
    );

    // The next rotation: d removes t; it carries s and x and drops y, which nothing seated.
    let removed = lab.aterm("d", &["members", "remove", &t_label]);
    lab.say(&format!(
        "d removes t ({t_label}): exit {}: {}",
        removed.code,
        one_line(&format!("{} {}", removed.stdout, removed.stderr))
    ));
    let wraps = |mac: &str| -> Option<bool> {
        let recipient = aterm_secrets::age::Recipient::parse(&recs[mac]).ok()?;
        let store = client::load_records(&lab.paths("d")).ok()?;
        Some(store.log()?.wraps_maximal_audience_to(
            &aterm_secrets::membership_log::EpochAudience::Everyone,
            &recipient,
        ))
    };
    let carried = lab.until("the rotation carries s and x", || {
        for mac in SEATLESS {
            let _ = lab.aterm(mac, &["secrets", "sync"]);
        }
        (wraps("s") == Some(true) && wraps("x") == Some(true) && wraps("y") == Some(false))
            .then_some(())
    });
    let y_status = lab.aterm("y", &["secrets", "status"]);
    push(
        &mut checks,
        Check {
            pass: removed.code == 0 && carried.is_ok(),
            name: "rotation-carries-seats",
            said: format!(
                "after d removed t, d's newest [everyone] epoch wraps s: {:?}, x: {:?}, y (never \
             seated): {:?}; s reads {:?}, x {:?}, y {:?}, and y's `secrets status` says {:?}",
                wraps("s"),
                wraps("x"),
                wraps("y"),
                client::seat(&lab.paths("s")),
                client::seat(&lab.paths("x")),
                client::seat(&lab.paths("y")),
                one_line(&format!("{} {}", y_status.stdout, y_status.stderr))
                    .rsplit("membership:")
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            ),
        },
    );

    // y, dropped by that rotation with no act against it, reads itself out and says so; t,
    // removed, says revoked (f204).
    let membership_line = |mac: &str| -> String {
        let status = lab.aterm(mac, &["secrets", "status"]);
        status
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("membership: "))
            .map_or_else(
                || one_line(&format!("{} {}", status.stdout, status.stderr)),
                str::to_owned,
            )
    };
    let mut y_said = String::new();
    let y_out = lab.until("y reads itself dropped", || {
        let _ = lab.aterm("y", &["secrets", "sync"]);
        y_said = membership_line("y");
        (client::seat(&lab.paths("y")) == client::Seat::Out && y_said.starts_with("dropped"))
            .then_some(())
    });
    push(
        &mut checks,
        Check {
            pass: y_out.is_ok() && !y_said.contains("revoked"),
            name: "dropped-not-revoked",
            said: format!(
                "y reads {:?}, and its `secrets status` says {y_said:?}",
                client::seat(&lab.paths("y"))
            ),
        },
    );
    let mut t_said = String::new();
    let t_out = lab.until("t reads its removal", || {
        let _ = lab.aterm("t", &["secrets", "sync"]);
        t_said = membership_line("t");
        t_said.starts_with("revoked").then_some(())
    });
    push(
        &mut checks,
        Check {
            pass: t_out.is_ok(),
            name: "removed-says-revoked",
            said: format!(
                "t reads {:?}, and its `secrets status` says {t_said:?}",
                client::seat(&lab.paths("t"))
            ),
        },
    );
    Ok(checks)
}

/// `swarm e2e`.
pub fn cmd(args: &[String]) -> Res {
    stop::install();
    let opts = parse(args)?;
    if !cfg!(debug_assertions) {
        return Err(Fail::exit(
            3,
            "swarm e2e needs a debug lab image: it holds the vault's lock by the product's test seam",
        ));
    }
    let aterm = crate::instance::resolve_aterm(opts.aterm).map_err(|e| Fail::exit(3, e))?;
    let world = World::new(&aterm, opts.keep).map_err(Fail::die)?;
    let ports = world::free_ports(3).map_err(|e| Fail::die(format!("free ports: {e}")))?;
    let ports: Vec<u16> = ports
        .into_iter()
        .filter(|p| *p != aterm_net::fleet_member::DEFAULT_PORT && *p != 7484)
        .collect();
    let [d, t, vault, ..] = ports.as_slice() else {
        return Err(Fail::die("too few free loopback ports"));
    };
    let mut lab = Lab {
        world,
        t0: Instant::now(),
        insts: BTreeMap::new(),
        fleet_ports: BTreeMap::from([("d", *d), ("t", *t)]),
        vault_port: *vault,
        signer: String::new(),
        kids: Vec::new(),
        lifelines: Vec::new(),
    };
    println!(
        "aterm-lab swarm e2e: drives by roster label under the vault's lock, and seatless Macs, \
         on this Mac's loopback; worlds under {}",
        lab.world.base.display()
    );
    let checks = run(&mut lab).map_err(|f| {
        if f.code == 1 {
            Fail { code: 3, ..f }
        } else {
            f
        }
    })?;
    let failed = checks.iter().filter(|c| !c.pass).count();
    println!(
        "{} of {} checks held, {:.1}s",
        checks.len() - failed,
        checks.len(),
        lab.t0.elapsed().as_secs_f64()
    );
    drop(lab);
    Ok(i32::from(failed > 0))
}
