// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `member HOME LISTEN OUT [--allow NAME] [--advertise SERVICE]`: the Allowing member of `lab mac
//! nux`'s `vault-phrase` row (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10), the ALab member that
//! lets the guest's joiner in by Allow and a phrase.
//!
//! It runs F10's members desk (`aterm_secrets::members::desk`), the code the window's `ask`
//! handler runs, on the custody under HOME (in the row, the scratch home of a member that is no
//! admin, enrolled in the vault's Commons): an `ask`-only listener at LISTEN hands each asking
//! Mac's stream to the desk, and a run that lets a Mac in posts the records its Allow signed
//! (where the vault has a membership log, F15) and that Mac's request with its `Allowed` to
//! HOME's vault server, as the window does. It offers no fleet op (its fleet secret is fresh and
//! held by no one). The first Mac that asks (the one named NAME, with `--allow`) gets one Allow,
//! pressed as soon as its request is heard, and the phrase the window would show in its sheet is
//! written to OUT: the shipped member shows a phrase on its own screen alone (§3.3.5), and the
//! lane reads no window, so this lab member hands the phrase to the lane, as `lab swarm
//! partition` presses Allow in its own process.
//!
//! Without `--advertise`, LISTEN must be a loopback address and the member registers no Bonjour
//! service. With `--advertise SERVICE`, SERVICE must be a lab's members service
//! (`aterm_secrets::discover::lab_service`: `_alab-lab-<nonce>._tcp`, never `_alab._tcp`, so no
//! live ALab Mac browses it) and LISTEN may be the guest's own address on its network (never a
//! wildcard): the member registers its ask port under SERVICE as the node job registers a
//! member's fleet port ([`advert_args`]: `dns-sd -R <alab-8 hex> SERVICE local <port> v=1
//! role=member fleet=<port> window=1`), so a joiner whose `ATERM_LAB_MEMBER_SERVICE` names
//! SERVICE finds it by `discover::browse_members` and dials it at the address that browse
//! resolves.
//!
//! OUT's lines (each `<kind> <epoch ms> …`, read by `mac_nux::phrase::read_desk`):
//! `member listen=<addr> name=<name>` once it serves, `refused <why>` when it cannot (no
//! Commons control, or one that may not allow), `heard id=<id> name=<name> code=<code>
//! from=<ip>` for each request, `phrase id=<id> name=<name> <word> <word> <word>` for its Allow,
//! `allow-failed <why>`, `let-in name=<name>` once a run's bundle reached the new Mac (it
//! acknowledged it, or asked for no acknowledgement), `sent-unacknowledged name=<name> <why>`
//! for one it never acknowledged (the window logs it sent, not let in), `posted ok` or `posted
//! failed <why>` for the request it posted, `asked from=<ip>` for an exchange that let no Mac
//! in, and `ask-ended from=<ip> <why>` for one that ended in an error.
//! `advertised instance=<name> service=<service> port=<port>` records a running registration;
//! a registration that cannot start is recorded as `refused`.

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_net::fleet::{AskHandler, Duplex, FleetOp, FleetScope, FleetSecret, Listening};
use aterm_secrets::client::{self, Paths};
use aterm_secrets::members::desk::{self, Delivered, Desk, Local, RowState};
use aterm_types::MutexExt as _;

use super::Out;

/// How long the member waits for its Commons state to read back as one that may allow (its
/// enrolment, or a founder's `secrets create`, wrote it): a hang detector, ended at once when it
/// reads.
const LOCAL_WITHIN: Duration = Duration::from_secs(60);

/// How often the member reads its Commons state while it waits, and looks at its desk.
const LOOK_EVERY: Duration = Duration::from_millis(50);

/// The connections the listener holds at once: one joiner asks, again, and runs its phrase.
const SLOTS: usize = 4;

/// What `member` was asked.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Ask {
    /// The member's home, whose vault custody the desk signs with.
    pub(crate) home: PathBuf,
    /// The listener's address: loopback, or with [`Self::advertise`] the guest's own address.
    pub(crate) listen: SocketAddr,
    /// The record.
    pub(crate) out: String,
    /// The one machine name whose request gets the Allow (`None`: the first one heard).
    pub(crate) allow: Option<String>,
    /// The lab members service the ask port is registered under (`None`: none).
    pub(crate) advertise: Option<String>,
}

impl Ask {
    /// `HOME LISTEN OUT [--allow NAME] [--advertise SERVICE]` (the words after `member`).
    ///
    /// # Errors
    /// A missing or stray word; a SERVICE that is no lab members service; or a LISTEN that is no
    /// `address:port`, that is no loopback one without `--advertise`, or that is a wildcard or
    /// multicast one with it: the member never listens where nothing names it.
    pub(crate) fn parse(args: &[String]) -> Result<Ask, String> {
        let usage = "member HOME LISTEN OUT [--allow NAME] [--advertise SERVICE]";
        let mut words = Vec::new();
        let mut allow = None;
        let mut advertise = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "--allow" {
                allow = Some(args.next().ok_or("--allow takes a machine name")?.clone());
            } else if arg == "--advertise" {
                let service = args.next().ok_or("--advertise takes a service")?;
                if !aterm_secrets::discover::lab_service(service) {
                    return Err(format!(
                        "member: {service} is no lab members service (_alab-lab-<1 to 6 \
                         lowercase letters or digits>._tcp): the lab's member never advertises \
                         a service the live fleet browses"
                    ));
                }
                advertise = Some(service.clone());
            } else if arg.starts_with("--") {
                return Err(format!("member: unknown option {arg} ({usage})"));
            } else {
                words.push(arg.clone());
            }
        }
        let [home, listen, out] = <[String; 3]>::try_from(words).map_err(|_| usage.to_owned())?;
        let listen: SocketAddr = listen
            .parse()
            .map_err(|_| format!("member: {listen} is no address:port"))?;
        let ip = listen.ip();
        if advertise.is_none() && !ip.is_loopback() {
            return Err(format!(
                "member: {listen} is not loopback: without --advertise the member listens where \
                 nothing beyond the guest reaches it"
            ));
        }
        if ip.is_unspecified() || ip.is_multicast() {
            return Err(format!(
                "member: {listen} is no one address: the member listens at the address its \
                 advert resolves to alone"
            ));
        }
        Ok(Ask {
            home: PathBuf::from(home),
            listen,
            out,
            allow,
            advertise,
        })
    }
}

/// The `dns-sd` arguments that register the member's ask port `port` under the lab members
/// service `service` as `instance`: the node job's `_alab._tcp` registration
/// (`aterm_agent::node_service::alab_tcp_dns_sd_args`, its instance, port and TXT as a window's
/// member advertises them) with `service` in `_alab._tcp`'s place.
pub(crate) fn advert_args(service: &str, instance: &str, port: u16) -> Vec<String> {
    use aterm_agent::node_service::{ALAB_TCP_SERVICE, AlabTcpTxt, NodeRole};
    let txt = AlabTcpTxt {
        role: NodeRole::Member,
        fleet_port: port,
        window: true,
    };
    aterm_agent::node_service::alab_tcp_dns_sd_args(instance, txt)
        .into_iter()
        .map(|word| {
            if word == ALAB_TCP_SERVICE {
                service.to_owned()
            } else {
                word
            }
        })
        .collect()
}

/// Register the ask port `port` under `service` ([`advert_args`], a fresh `alab-<8 hex>`
/// instance): the running `dns-sd`, which holds the registration while it lives (its output in
/// `home/member.advert`), and the instance.
///
/// # Errors
/// The output file could not be made, or `dns-sd` could not start.
fn register(
    home: &std::path::Path,
    service: &str,
    port: u16,
) -> Result<(aterm_uds::spawnfd::Child, String), String> {
    let instance = aterm_agent::node_service::random_alab_instance_name();
    let log = home.join("member.advert");
    let _ = std::fs::remove_file(&log);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = opts
        .open(&log)
        .map_err(|e| format!("{}: {e}", log.display()))?;
    let err = file
        .try_clone()
        .map_err(|e| format!("{}: {e}", log.display()))?;
    let child = aterm_uds::spawnfd::command(aterm_agent::node_service::DNS_SD)
        .args(advert_args(service, &instance, port))
        .stdin(aterm_uds::spawnfd::Stdio::null())
        .stdout(aterm_uds::spawnfd::Stdio::from(file))
        .stderr(aterm_uds::spawnfd::Stdio::from(err))
        .spawn()
        .map_err(|e| format!("dns-sd -R {instance} {service}: {e}"))?;
    Ok((child, instance))
}

/// Serve as the Allowing member until killed ([`Ask`], the module docs).
///
/// # Errors
/// The record could not be opened, HOME is no member that may allow, or the
/// listener could not bind; each but the first is also a `refused` line.
pub(crate) fn member(ask: &Ask) -> Result<(), String> {
    let out = Arc::new(Out::open(&ask.out)?);
    let refuse = |why: String| {
        out.line("refused", &flat(&why));
        why
    };
    let paths = Paths::for_home(&ask.home);
    let local = wait_local(&paths).map_err(refuse)?;
    let name = local.name().to_owned();
    let desk = Arc::new(Mutex::new(Desk::new(local)));
    let listener =
        TcpListener::bind(ask.listen).map_err(|e| refuse(format!("{}: {e}", ask.listen)))?;
    let bound = listener
        .local_addr()
        .map_err(|e| refuse(format!("{}: {e}", ask.listen)))?;
    // Held while the member serves, which it does until it is killed: the registration lives as
    // long as its `dns-sd`.
    let _advert = match &ask.advertise {
        Some(service) => {
            let (child, instance) = register(&ask.home, service, bound.port()).map_err(refuse)?;
            out.line(
                "advertised",
                &format!(
                    "instance={instance} service={service} port={}",
                    bound.port()
                ),
            );
            Some(child)
        }
        None => None,
    };
    let listening = Listening {
        secret: fresh_secret().map_err(refuse)?,
        scope: FleetScope::Facts,
        node: None,
        serves: &[],
        slots: SLOTS,
        per_source: aterm_net::fleet::per_source(SLOTS),
        lifetime: aterm_net::fleet::SERVED_WITHIN,
    };
    let handler = ask_handler(Arc::clone(&desk), paths, Arc::clone(&out));
    let running = AtomicBool::new(true);
    out.line("member", &format!("listen={bound} name={name}"));
    std::thread::scope(|scope| -> Result<(), String> {
        scope.spawn(|| {
            let none = Arc::new(aterm_net::fleet::Denied::default());
            aterm_net::fleet::serve_offering_with_ask(
                &listener,
                &listening,
                Arc::new(|| -> (FleetScope, &'static [FleetOp]) { (FleetScope::Facts, &[]) }),
                Arc::new(move || Arc::clone(&none)),
                Some(handler),
                &running,
                // No fleet op is offered, so none is handed over.
                |_, _| Ok(()),
                |_| {},
            );
        });
        press(&desk, &out, ask.allow.as_deref());
    })
}

/// HOME's membership once it reads as a member that may allow, within [`LOCAL_WITHIN`].
fn wait_local(paths: &Paths) -> Result<Local, String> {
    let started = Instant::now();
    loop {
        let why = match Local::of(paths) {
            Ok(Some(local)) if local.can_allow() => return Ok(local),
            Ok(Some(_)) => {
                "neither its membership log seats this member nor a Commons roster binds it, so \
                 it cannot allow"
                    .to_owned()
            }
            Ok(None) => {
                "this Mac reads as no member: no vault, one not applied or still busy, no seat in \
                 its membership log, or no pinned Commons anchor"
                    .to_owned()
            }
            Err(error) => format!("this member's membership: {error}"),
        };
        if started.elapsed() >= LOCAL_WITHIN {
            return Err(format!("{why} ({} s on)", LOCAL_WITHIN.as_secs()));
        }
        std::thread::sleep(LOOK_EVERY);
    }
}

/// `text` on one line, its words one space apart: a record line's tail.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A fleet secret no one else holds: the listener proves it to no one and serves no op by it.
fn fresh_secret() -> Result<FleetSecret, String> {
    let mut bytes = [0u8; FleetSecret::MAX_LEN];
    aterm_uds::rand::fill(&mut bytes).map_err(|e| format!("entropy: {e}"))?;
    FleetSecret::new(&bytes).map_err(|_| "a fleet secret of the wrong length".to_owned())
}

/// The listener's `ask` handler: the asking Mac's stream goes to `desk`, which hands what
/// [`client::hand_over`] gives (the membership log where the vault has one, else F10's files),
/// and a run that let it in posts the records it signed, where it signed any, and then its
/// request with the `Allowed` to HOME's vault server, off the exchange's thread, as the window's
/// handler does.
fn ask_handler(desk: Arc<Mutex<Desk>>, paths: Paths, out: Arc<Out>) -> AskHandler {
    Arc::new(
        move |stream: &mut dyn Duplex, peer: SocketAddr| -> std::io::Result<()> {
            let bundle = |handing: &desk::Handing<'_>| client::hand_over(&paths, handing);
            match desk::serve(&desk, stream, peer.ip(), &bundle) {
                Ok(Some(let_in)) => {
                    let (kind, rest) = ran(let_in.allowed.name(), &let_in.delivered);
                    out.line(kind, &rest);
                    let (paths, out) = (paths.clone(), Arc::clone(&out));
                    std::thread::spawn(move || {
                        if let_in.logged
                            && let Err(error) = client::post_records_to_servers(&paths)
                        {
                            out.line("records", &format!("failed {}", flat(&error.to_string())));
                        }
                        match client::post_allowed(&paths, &let_in.allowed) {
                            Ok(()) => out.line("posted", "ok"),
                            Err(error) => {
                                out.line("posted", &format!("failed {}", flat(&error.to_string())));
                            }
                        }
                    });
                }
                Ok(None) => out.line("asked", &format!("from={}", peer.ip())),
                Err(error) => out.line(
                    "ask-ended",
                    &format!("from={} {}", peer.ip(), flat(&error.to_string())),
                ),
            }
            Ok(())
        },
    )
}

/// The record line for a run that sent Mac `name` its bundle, by how it was `delivered`: `let-in`
/// once the bundle reached it, `sent-unacknowledged` (with why) for one it never acknowledged,
/// which the window logs as sent, not let in. Either way the records and the `Allowed` are
/// posted, as the window posts them.
fn ran(name: &str, delivered: &Delivered) -> (&'static str, String) {
    match delivered {
        Delivered::Acknowledged | Delivered::Written => ("let-in", format!("name={name}")),
        Delivered::Unacknowledged(why) => {
            ("sent-unacknowledged", format!("name={name} {}", flat(why)))
        }
    }
}

/// Look at the desk every [`LOOK_EVERY`]: write each request heard, and press Allow once, for
/// the first request asking as `allow` (any, when `None`), writing the phrase it shows.
fn press(desk: &Mutex<Desk>, out: &Out, allow: Option<&str>) -> ! {
    let mut heard = BTreeSet::new();
    let mut pressed = false;
    loop {
        {
            let mut desk = desk.lock_or_recover();
            let now = aterm_time::Instant::now();
            let rows = desk.rows(now);
            for row in &rows {
                if heard.insert(row.id) {
                    out.line(
                        "heard",
                        &format!(
                            "id={} name={} code={} from={}",
                            row.id, row.name, row.code, row.from
                        ),
                    );
                }
            }
            let asking = rows.iter().find(|row| {
                row.state == RowState::Asking && allow.is_none_or(|name| name == row.name)
            });
            if let (false, Some(row)) = (pressed, asking) {
                pressed = true;
                match desk.allow(row.id, now) {
                    Ok(phrase) => match desk.words(phrase, now) {
                        Some(words) => out.line(
                            "phrase",
                            &format!("id={} name={} {}", row.id, row.name, words.join(" ")),
                        ),
                        None => out.line("allow-failed", "the phrase ended before it showed"),
                    },
                    Err(error) => out.line("allow-failed", &flat(&error.to_string())),
                }
            }
        }
        std::thread::sleep(LOOK_EVERY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    /// Without `--advertise` the member listens on loopback alone: an address the guest's
    /// network reaches (any interface, the guest's own address on tart's network, IPv6's any) is
    /// refused before anything binds.
    #[test]
    fn the_member_listens_on_loopback_alone() {
        let ask = Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "--allow", "mb"])).unwrap();
        assert_eq!(
            ask,
            Ask {
                home: PathBuf::from("/s"),
                listen: "127.0.0.1:7486".parse().unwrap(),
                out: "/o".into(),
                allow: Some("mb".into()),
                advertise: None,
            }
        );
        assert!(Ask::parse(&args(&["/s", "[::1]:7486", "/o"])).is_ok());
        for beyond in ["0.0.0.0:7486", "192.168.64.5:7486", "[::]:7486"] {
            let refused = Ask::parse(&args(&["/s", beyond, "/o"])).unwrap_err();
            assert!(refused.contains("not loopback"), "{beyond}: {refused}");
        }
        assert!(Ask::parse(&args(&["/s", "127.0.0.1", "/o"])).is_err());
        assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486"])).is_err());
        assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "x"])).is_err());
        assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "--allow"])).is_err());
        assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "--loud"])).is_err());
    }

    /// A run whose bundle the new Mac never acknowledged is written as sent, not as a Mac let in,
    /// so `lab mac nux` counts no join the new Mac never took; one it acknowledged, or one from a
    /// joiner that asked for no acknowledgement, is a Mac let in.
    ///
    /// NEGATIVE CONTROL: write `let-in` for every run (`ran` ignoring `delivered`):
    /// `left: ("let-in", "name=mb")`, `right: ("sent-unacknowledged", "name=mb the other Mac
    /// closed the join exchange")`.
    #[test]
    fn a_bundle_the_new_mac_never_acknowledged_is_no_mac_let_in() {
        assert_eq!(
            ran(
                "mb",
                &Delivered::Unacknowledged("the other Mac  closed\nthe join exchange".into())
            ),
            (
                "sent-unacknowledged",
                "name=mb the other Mac closed the join exchange".to_owned()
            )
        );
        for delivered in [Delivered::Acknowledged, Delivered::Written] {
            assert_eq!(
                ran("mb", &delivered),
                ("let-in", "name=mb".to_owned()),
                "{delivered:?}"
            );
        }
    }

    /// With `--advertise` the member listens at the guest's own address and registers there
    /// under a lab members service alone: `_alab._tcp`, the vault's service or any name that is
    /// no lab one is refused, and so is a wildcard or multicast address, before anything binds or
    /// registers. Its registration is the node job's, with the lab service in `_alab._tcp`'s
    /// place.
    ///
    /// NEGATIVE CONTROL (2026-10-29): `Ask::parse` taking any `--advertise` word (its
    /// `lab_service` check removed) fails here: `_alab._tcp was taken`.
    #[test]
    fn an_advertising_member_registers_a_lab_service_alone_at_one_address() {
        let lab = "_alab-lab-k3x9q2._tcp";
        let ask = Ask::parse(&args(&[
            "/s",
            "192.168.64.5:7486",
            "/o",
            "--advertise",
            lab,
        ]))
        .unwrap();
        assert_eq!(ask.listen, "192.168.64.5:7486".parse().unwrap());
        assert_eq!(ask.advertise.as_deref(), Some(lab));
        assert!(Ask::parse(&args(&["/s", "127.0.0.1:7486", "/o", "--advertise", lab])).is_ok());
        for service in [
            "_alab._tcp",
            "_alab-vault._tcp",
            "_alab-lab-._tcp",
            "_http._tcp",
        ] {
            let said = Ask::parse(&args(&[
                "/s",
                "192.168.64.5:7486",
                "/o",
                "--advertise",
                service,
            ]));
            assert!(
                said.as_ref()
                    .is_err_and(|e| e.contains("no lab members service")),
                "{service} was taken: {said:?}"
            );
        }
        for wide in ["0.0.0.0:7486", "[::]:7486", "224.0.0.251:7486"] {
            let said = Ask::parse(&args(&["/s", wide, "/o", "--advertise", lab]));
            assert!(said.is_err(), "{wide} was taken: {said:?}");
        }
        assert!(Ask::parse(&args(&["/s", "192.168.64.5:7486", "/o", "--advertise"])).is_err());

        let shipped = aterm_agent::node_service::alab_tcp_dns_sd_args(
            "alab-1a2b3c4d",
            aterm_agent::node_service::AlabTcpTxt {
                role: aterm_agent::node_service::NodeRole::Member,
                fleet_port: 7486,
                window: true,
            },
        );
        let ours = advert_args(lab, "alab-1a2b3c4d", 7486);
        assert_eq!(
            ours,
            [
                "-R",
                "alab-1a2b3c4d",
                lab,
                "local",
                "7486",
                "v=1",
                "role=member",
                "fleet=7486",
                "window=1"
            ]
        );
        let differs: Vec<(&String, &String)> =
            shipped.iter().zip(&ours).filter(|(a, b)| a != b).collect();
        assert_eq!(
            differs,
            [(&"_alab._tcp".to_owned(), &lab.to_owned())],
            "only the service differs from the node job's registration"
        );
    }
}
