// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mail pushes over the mesh (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, §11 C3): with the
//! mesh on and `bus.log` hosted, one push per Mac this Mac's sessions wrote to
//! ([`aterm_link::federate::Routes`]), from this Mac's `bus.log` to that Mac's over the link the
//! overlay holds to it, or over one dialed for the mail alone, held in a table of its own the
//! overlay does not prune. Each pass (the job's watch tick) reads the membership log's standings
//! the mesh applied, routes by them, writes the hand-off the fabric bridge routes by beside
//! `bus.sock`, and wakes every push waiting out a back-off once a new link or the log came up.

use std::collections::BTreeSet;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use astream_broker::{StreamDial, StreamLink};
use aterm_link::federate::{Dest, Handoff, Routes, RoutesConfig};
use aterm_net::fleet::NodeIdentity;
use aterm_net::fleet::link::{Link, LinkConfig, Neighbours};

/// The principal of the node key a link's peer proved.
fn principal_of(link: &Link) -> String {
    astream_broker::identity_principal(link.peer().key.bytes())
}

/// A generation's mail pushes, the table of the links dialed for them, and what the last pass
/// wrote and saw. Dropped, every push stops and is joined and the mail links close.
pub struct MailLinks {
    routes: Routes,
    mail: Arc<Neighbours>,
    timer: Option<std::thread::JoinHandle<()>>,
    mesh: Arc<crate::Mesh>,
    overlay: Arc<Neighbours>,
    kept: Mutex<Kept>,
}

#[derive(Default)]
struct Kept {
    /// The hand-off last written, and where.
    written: Option<(PathBuf, Handoff)>,
    /// The peers linked, and whether `bus.log` was hosted, at the last pass.
    linked: BTreeSet<String>,
    hosted: bool,
    /// The last reason a hand-off could not be written, journaled once.
    said: Option<String>,
}

/// This Mac's own `bus.log`, as the mesh joined it now, adopted under this Mac's node key: what
/// every push reads from.
struct OwnBus {
    mesh: Arc<crate::Mesh>,
}

impl StreamDial for OwnBus {
    fn dial(&self) -> io::Result<Arc<dyn StreamLink>> {
        let host = self.mesh.bus().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "this Mac hosts no bus.log now")
        })?;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        drop(
            host.broker()
                .serve_stream(Arc::new(theirs), self.mesh.key())?,
        );
        Ok(Arc::new(ours))
    }
}

/// A peer's `bus.log` over one link: each dial opens a `Bus` stream on it.
struct PeerBus {
    link: Arc<Link>,
}

impl StreamDial for PeerBus {
    fn dial(&self) -> io::Result<Arc<dyn StreamLink>> {
        Ok(Arc::new(crate::logs::dial_bus(&self.link)?.0))
    }
}

impl MailLinks {
    /// Start the pushes of this generation, for this Mac (`me`) and its `mesh`, over the
    /// overlay's table `overlay` and a mail table of its own built like it, dialing seats at the
    /// addresses `hints` gives. `None` where the mail table's timer or the watch cannot start
    /// (said).
    pub fn start(
        me: &NodeIdentity,
        mesh: &Arc<crate::Mesh>,
        overlay: &Arc<Neighbours>,
        hints: crate::overlay::Hints,
    ) -> Option<Arc<Self>> {
        let cfg = overlay.config();
        let mail = Neighbours::new(
            me.clone().dialing_mesh(Some(Arc::clone(&cfg.verdict))),
            LinkConfig {
                verdict: Arc::clone(&cfg.verdict),
                serve: Arc::clone(&cfg.serve),
                note: Arc::clone(&cfg.note),
                clock: Arc::clone(&cfg.clock),
            },
        );
        let timer = match mail.spawn_timer() {
            Ok(timer) => timer,
            Err(e) => {
                aterm_log::warn!("mesh: the mail links' timer did not start: {e}");
                return None;
            }
        };
        let minting = Arc::clone(mesh);
        let (dest_mesh, dest_overlay, dest_mail) =
            (Arc::clone(mesh), Arc::clone(overlay), Arc::clone(&mail));
        let routes = Routes::start(RoutesConfig {
            me: mesh.principal().to_owned(),
            source: Arc::new(OwnBus {
                mesh: Arc::clone(mesh),
            }),
            mint: Arc::new(move |grant| {
                minting
                    .bus()
                    .ok_or_else(|| {
                        io::Error::new(io::ErrorKind::NotConnected, "this Mac hosts no bus.log now")
                    })?
                    .mint(grant)
            }),
            dest: Arc::new(move |node| {
                dest_of(node, &dest_mesh, &dest_overlay, &dest_mail, &hints)
            }),
            note: Arc::new(|line| aterm_log::info!("{line}")),
        });
        let routes = match routes {
            Ok(routes) => routes,
            Err(e) => {
                aterm_log::warn!("mesh: the mail pushes did not start: {e}");
                mail.stop();
                let _ = timer.join();
                return None;
            }
        };
        Some(Arc::new(Self {
            routes,
            mail,
            timer: Some(timer),
            mesh: Arc::clone(mesh),
            overlay: Arc::clone(overlay),
            kept: Mutex::new(Kept::default()),
        }))
    }

    /// One pass: route by the standings the mesh applied, write the hand-off where it changed,
    /// end mail links to keys the log took out, and wake every waiting push where a link or
    /// `bus.log` came up since the last pass.
    pub fn pass(&self) {
        self.mail.judge();
        let me = self.mesh.principal().to_owned();
        let Some(standings) = self.mesh.standings() else {
            return;
        };
        let handoff = Handoff {
            members: standings
                .current
                .iter()
                .map(aterm_link::federate::node_of)
                .chain([me.clone()])
                .collect(),
            moved: standings
                .rotated
                .iter()
                .map(|(old, new)| {
                    (
                        aterm_link::federate::node_of(old),
                        aterm_link::federate::node_of(new),
                    )
                })
                .collect(),
            me,
        };
        // Topics go to the Macs whose presence rows name one.
        self.routes.set(&handoff, &self.mesh.holders());
        let host = self.mesh.bus();
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let root = host.as_ref().map(|host| host.spec().root.clone());
        // A hand-off left where this node service no longer hosts `bus.log` goes.
        if let Some((was, _)) = kept.written.take_if(|(was, _)| root.as_ref() != Some(was)) {
            let _ = std::fs::remove_file(was.join(aterm_link::federate::HANDOFF_FILE));
        }
        if let Some(root) = root
            && kept
                .written
                .as_ref()
                .is_none_or(|(at, was)| *at != root || *was != handoff)
        {
            match handoff.write(&root) {
                Ok(()) => {
                    kept.said = None;
                    kept.written = Some((root, handoff));
                }
                Err(e) => {
                    // The bridge routes to no other Mac without a current hand-off: the last one
                    // goes, so it fails closed rather than route by members that changed.
                    let file = root.join(aterm_link::federate::HANDOFF_FILE);
                    let _ = std::fs::remove_file(&file);
                    kept.written = None;
                    let why = format!("{}: {e}", file.display());
                    if kept.said.as_deref() != Some(why.as_str()) {
                        aterm_log::warn!(
                            "mesh: the fabric's hand-off could not be written ({why}); the \
                             bridge routes to no other Mac until it is"
                        );
                        kept.said = Some(why);
                    }
                }
            }
        }
        let linked: BTreeSet<String> = self
            .overlay
            .links()
            .iter()
            .chain(self.mail.links().iter())
            .map(|link| principal_of(link))
            .collect();
        let hosted = host.is_some();
        let woke = (hosted && !kept.hosted) || linked.iter().any(|p| !kept.linked.contains(p));
        kept.linked = linked;
        kept.hosted = hosted;
        drop(kept);
        if woke {
            self.routes.kick();
        }
    }

    /// Ask every push to stop, without waiting: the links' close then ends any dial under way,
    /// and the drop joins them.
    pub fn stop_soon(&self) {
        self.routes.stop_soon();
        self.mail.stop();
    }
}

impl Drop for MailLinks {
    /// Stops the pushes, and takes the hand-off down: with no push running, the bridge routes to
    /// no other Mac (it fails closed on a missing hand-off) until the next node service writes
    /// one.
    fn drop(&mut self) {
        self.stop_soon();
        self.routes.stop();
        if let Some(timer) = self.timer.take() {
            let _ = timer.join();
        }
        let written = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .written
            .take();
        if let Some((root, _)) = written {
            let _ = std::fs::remove_file(root.join(aterm_link::federate::HANDOFF_FILE));
        }
    }
}

/// The topics this Mac's sessions hold, as its fabric bridge last named them on `bus.log`
/// ([`aterm_link::federate::topics_face`]): what its presence row says (§11 C3). None while the
/// mesh hosts no `bus.log`, the bridge named none, or the face cannot be read.
#[must_use]
pub fn held_topics(mesh: &crate::Mesh) -> Vec<String> {
    let face = aterm_link::federate::topics_face(mesh.principal());
    let read = || -> io::Result<Vec<String>> {
        let Some(host) = mesh.bus() else {
            return Ok(Vec::new());
        };
        let cap = host.mint(&format!("ro:{face}"))?;
        let (ours, theirs) = std::os::unix::net::UnixStream::pair()?;
        drop(host.broker().serve_stream(Arc::new(theirs), mesh.key())?);
        ours.set_read_timeout(Some(HELD_READ))?;
        let (mut client, _closer) = astream_broker::connect_stream(Arc::new(ours));
        client.attach_all([(cap.filter.as_str(), &cap.tag[..])])?;
        let (rows, _) = client.last_all(&face)?;
        Ok(rows
            .last()
            .and_then(|(_, _, body)| aterm_link::federate::topics_of(body))
            .unwrap_or_default()
            .into_iter()
            .collect())
    };
    read().unwrap_or_default()
}

/// How long a read of this Mac's own held topics waits on its `bus.log`, in process.
const HELD_READ: std::time::Duration = std::time::Duration::from_secs(5);

/// Where a push to `node` goes: a live link the overlay holds to it, else one the mail table
/// holds, else one dialed now at an address `hints` gives, its stream's ring read from the first
/// `Bus` stream. Each is a link whose peer proved a current seat's key
/// ([`crate::overlay::proves_a_seat`]), not merely `node`'s principal.
fn dest_of(
    node: &str,
    mesh: &crate::Mesh,
    overlay: &Arc<Neighbours>,
    mail: &Arc<Neighbours>,
    hints: &crate::overlay::Hints,
) -> io::Result<Dest> {
    let held = overlay
        .links()
        .into_iter()
        .chain(mail.links())
        .find(|link| principal_of(link) == node && crate::overlay::proves_a_seat(mesh, link));
    let link = match held {
        Some(link) => link,
        None => {
            let mut last = None;
            let mut linked = None;
            for (_, addr) in hints().into_iter().filter(|(to, _)| to == node) {
                match mail.dial(addr, crate::overlay::DIAL_WITHIN) {
                    Ok(link)
                        if principal_of(&link) == node
                            && crate::overlay::proves_a_seat(mesh, &link) =>
                    {
                        linked = Some(link);
                        break;
                    }
                    Ok(link) => {
                        link.close("the mail link reached another Mac than the one it dialed");
                        last = Some(format!("{addr} is another Mac now"));
                    }
                    Err(e) => last = Some(format!("{addr}: {}", e.error)),
                }
            }
            linked.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotConnected,
                    last.unwrap_or_else(|| format!("no address of {node} is known")),
                )
            })?
        }
    };
    let (_, grants) = crate::logs::dial_bus(&link)?;
    Ok(Dest {
        dial: Arc::new(PeerBus { link }),
        grants,
    })
}
