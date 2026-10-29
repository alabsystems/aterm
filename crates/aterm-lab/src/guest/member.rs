// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `member HOME LISTEN OUT [--allow NAME]`: the Allowing member of `lab mac nux`'s
//! `vault-phrase` row (`docs/DESIGN-fluid-swarm-2026-10-28.md` F10), the ALab member that lets
//! the guest's joiner in by Allow and a phrase.
//!
//! It runs F10's members desk (`aterm_secrets::members::desk`), the code the window's `ask`
//! handler runs, on the custody under HOME (in the row, the scratch home of a member that is no
//! admin, enrolled in the vault's Commons): an `ask`-only listener at LISTEN, which must be a
//! loopback address, hands each asking Mac's stream to the desk, and a run that lets a Mac in
//! posts the records its Allow signed (where the vault has a membership log, F15) and that
//! Mac's request with its `Allowed` to HOME's vault server, as the window does. It offers no
//! fleet op (its fleet secret is fresh and held by no one) and registers no Bonjour service.
//! The first Mac that asks (the one named NAME, with `--allow`) gets one Allow, pressed as
//! soon as its request is heard, and the phrase the window would show in its sheet is written
//! to OUT: the shipped member shows a
//! phrase on its own screen alone (§3.3.5), and the lane reads no window, so this lab member
//! hands the phrase to the lane, as `lab swarm partition` presses Allow in its own process.
//!
//! OUT's lines (each `<kind> <epoch ms> …`, read by `mac_nux::phrase::read_desk`):
//! `member listen=<addr> name=<name>` once it serves, `refused <why>` when it cannot (no
//! Commons control, or one that may not allow), `heard id=<id> name=<name> code=<code>
//! from=<ip>` for each request, `phrase id=<id> name=<name> <word> <word> <word>` for its Allow,
//! `allow-failed <why>`, `let-in name=<name>` once a run sent its bundle, `posted ok` or
//! `posted failed <why>` for the request it posted, `asked from=<ip>` for an exchange that let
//! no Mac in, and `ask-ended from=<ip> <why>` for one that ended in an error.

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aterm_net::fleet::{AskHandler, Duplex, FleetOp, FleetScope, FleetSecret, Listening};
use aterm_secrets::client::{self, Paths};
use aterm_secrets::members::desk::{self, Desk, Local, RowState};
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
    /// The listener's address: loopback alone.
    pub(crate) listen: SocketAddr,
    /// The record.
    pub(crate) out: String,
    /// The one machine name whose request gets the Allow (`None`: the first one heard).
    pub(crate) allow: Option<String>,
}

impl Ask {
    /// `HOME LISTEN OUT [--allow NAME]` (the words after `member`).
    ///
    /// # Errors
    /// A missing or stray word, or a LISTEN that is no loopback `address:port`: the member never
    /// listens where the guest's network reaches it.
    pub(crate) fn parse(args: &[String]) -> Result<Ask, String> {
        let usage = "member HOME LISTEN OUT [--allow NAME]";
        let mut words = Vec::new();
        let mut allow = None;
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if arg == "--allow" {
                allow = Some(args.next().ok_or("--allow takes a machine name")?.clone());
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
        if !listen.ip().is_loopback() {
            return Err(format!(
                "member: {listen} is not loopback: the member listens where nothing beyond the \
                 guest reaches it"
            ));
        }
        Ok(Ask {
            home: PathBuf::from(home),
            listen,
            out,
            allow,
        })
    }
}

/// Serve as the Allowing member until killed ([`Ask`], the module docs).
///
/// # Errors
/// The record could not be opened, HOME holds no Commons control that may allow, or the
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
                &|| -> (FleetScope, &'static [FleetOp]) { (FleetScope::Facts, &[]) },
                &|| Arc::clone(&none),
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

/// HOME's Commons state once it reads as a member that may allow, within [`LOCAL_WITHIN`].
fn wait_local(paths: &Paths) -> Result<Local, String> {
    let started = Instant::now();
    loop {
        let why = match Local::of(paths) {
            Ok(Some(local)) if local.can_allow() => return Ok(local),
            Ok(Some(_)) => "no Commons roster binds this member, so it cannot allow".to_owned(),
            Ok(None) => "this member holds no Commons control".to_owned(),
            Err(error) => format!("this member's Commons state: {error}"),
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
    std::io::Read::read_exact(
        &mut std::fs::File::open("/dev/urandom").map_err(|e| format!("/dev/urandom: {e}"))?,
        &mut bytes,
    )
    .map_err(|e| format!("/dev/urandom: {e}"))?;
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
                    out.line("let-in", &format!("name={}", let_in.allowed.name()));
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

    /// The member listens on loopback alone: an address the guest's network reaches (any
    /// interface, the guest's own address on tart's network, IPv6's any) is refused before
    /// anything binds.
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
}
