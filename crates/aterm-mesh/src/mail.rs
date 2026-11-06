// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The mail pushes over the mesh (`docs/DESIGN-alab-one-mesh-2026-11-05.md` §4.6, §11 C3): with the
//! mesh on and `bus.log` hosted, one push per Mac this Mac's sessions wrote to
//! ([`aterm_link::federate::Routes`]), from this Mac's `bus.log` to that Mac's over the link the
//! overlay holds to it, or over one dialed for the mail alone, held in a table of its own the
//! overlay does not prune. Each pass (the job's watch tick) reads the membership log's standings
//! the mesh applied, routes by them, writes the hand-off the fabric bridge routes by beside
//! `bus.sock`, and the reader's ring that reads the mail of this Mac's retired nodes beside it
//! (`aterm_link::federate::OLD_READS_FILE`, §11 C3b), vouches with this Mac's node key for its person key
//! (`aterm_link::person_key::vouch`, §11 C3c), and wakes every push waiting out a back-off once a
//! new link or the log came up. It also relays every halt this Mac's `bus.log` holds to every
//! other member, so a halt floods the mesh (§11 C4), and names in the hand-off each node the log
//! removed or rotated away, whose halts the bridge lifts.

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

/// The hand-off for the Mac `me` from the standings the mesh applied: the current members, this
/// Mac among them unless the log took it out (one the log has not seated yet counts in); each
/// rotated node and the one its seat holds now; and every node gone, each key the log removed or
/// a `Node` record retired that no member's key names, this Mac's own included, whose halts the
/// bridge lifts (§4.11, §11 C4: "No halt outlives its origin", on the removed Mac too).
#[must_use]
pub fn handoff_of(me: String, standings: &crate::Standings) -> Handoff {
    let node_of = aterm_link::federate::node_of;
    let out: BTreeSet<String> = standings
        .removed
        .iter()
        .chain(&standings.retired)
        .map(node_of)
        .collect();
    let mut members: BTreeSet<String> = standings.current.iter().map(node_of).collect();
    if !out.contains(&me) {
        members.insert(me.clone());
    }
    Handoff {
        moved: standings
            .rotated
            .iter()
            .map(|(old, new)| (node_of(old), node_of(new)))
            .collect(),
        gone: out.difference(&members).cloned().collect(),
        members,
        me,
    }
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
    /// The hand-off built at the last standings generation, and that generation.
    handoff: Option<(u64, Handoff)>,
    /// The hand-off last written, and where.
    written: Option<(PathBuf, Handoff)>,
    /// The peers linked, and whether `bus.log` was hosted, at the last pass.
    linked: BTreeSet<String>,
    hosted: bool,
    /// The node the fabric bridge's state dir named at the last pass (§11 C3b).
    bridge_node: Option<String>,
    /// The last reason a hand-off could not be written, journaled once.
    said: Option<String>,
    /// The last reason the person key could not be vouched for, journaled once.
    vouch_said: Option<String>,
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
        let (minting, seats_mesh, hosting) = (Arc::clone(mesh), Arc::clone(mesh), Arc::clone(mesh));
        let (dest_mesh, dest_overlay, dest_mail) =
            (Arc::clone(mesh), Arc::clone(overlay), Arc::clone(&mail));
        let routes = Routes::start(RoutesConfig {
            me: mesh.principal().to_owned(),
            key: mesh.key(),
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
            seats: Arc::new(move |owner| seats_mesh.still_seats(owner)),
            // The bridge aterm launches on this bus.log keeps its state beside it
            // (`aterm_link::enable`'s `--state`), where the own-inbox move reads which node it
            // runs as and what it handed over.
            bridge_state: Arc::new(move || {
                hosting.bus().map(|host| {
                    host.spec()
                        .root
                        .join(aterm_link::federate::BRIDGE_STATE_DIR)
                })
            }),
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
        let generation = self
            .mesh
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        let cached = self
            .kept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handoff
            .as_ref()
            .filter(|(was, _)| *was == generation)
            .map(|(_, handoff)| handoff.clone());
        let handoff = if let Some(handoff) = cached {
            handoff
        } else {
            let me = self.mesh.principal().to_owned();
            let Some(standings) = self.mesh.standings() else {
                return;
            };
            let handoff = handoff_of(me, &standings);
            self.kept
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .handoff = Some((generation, handoff.clone()));
            handoff
        };
        self.vouch();
        let host = self.mesh.bus();
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        let root = host.as_ref().map(|host| host.spec().root.clone());
        // The node the bridge state dir names: a bridge that names another there than the node it
        // ran as opens the move of that node's unread mail (§11 C3b,
        // `aterm_link::federate::Lane::Inbox`), which is kicked, not left to its back-off.
        let bridge_node = root.as_ref().and_then(|root| {
            aterm_link::state::StateDir::peek(root.join(aterm_link::federate::BRIDGE_STATE_DIR))
                .node()
        });
        // A hand-off left where this node service no longer hosts `bus.log` goes.
        if let Some((was, _)) = kept.written.take_if(|(was, _)| root.as_ref() != Some(was)) {
            remove_handoff(&was);
        }
        if let Some(root) = root
            && kept
                .written
                .as_ref()
                .is_none_or(|(at, was)| *at != root || *was != handoff)
        {
            // The reader's ring first, so a bridge that reads this hand-off finds it: the reads of
            // the mail lanes of this Mac's retired nodes, where the mail the own-inbox move took
            // came from (§11 C3b).
            let reads = host.as_ref().map_or(Ok(()), |host| {
                aterm_link::federate::write_old_reads(&root, &handoff, &|grant| host.mint(grant))
            });
            match handoff.write(&root) {
                Ok(()) => match reads {
                    Ok(()) => {
                        kept.said = None;
                        kept.written = Some((root, handoff.clone()));
                    }
                    Err(e) => {
                        // The hand-off routes; the ring is written again on the next pass.
                        let file = root.join(aterm_link::federate::OLD_READS_FILE);
                        let why = format!("{}: {e}", file.display());
                        if kept.said.as_deref() != Some(why.as_str()) {
                            aterm_log::warn!(
                                "mesh: the fabric's reader's ring could not be written ({why}); \
                                 until it is, a receipt for mail moved across this Mac's key \
                                 rotation names it by its offset here"
                            );
                            kept.said = Some(why);
                        }
                    }
                },
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
        let woke = (hosted && !kept.hosted)
            || linked.iter().any(|p| !kept.linked.contains(p))
            || (bridge_node.is_some() && bridge_node != kept.bridge_node);
        kept.linked = linked;
        kept.hosted = hosted;
        kept.bridge_node = bridge_node;
        drop(kept);
        // The pushes after the hand-off, so the bridge reads the nodes this Mac held before
        // ahead of the first record the own-inbox move brings it (§11 C3b). Topics go to the Macs
        // whose presence rows name one.
        self.routes.set(&handoff, &self.mesh.holders());
        if woke {
            self.routes.kick();
        }
    }

    /// How many times a pass kicked the pushes ([`aterm_link::federate::Routes::kick`]).
    #[cfg(test)]
    pub(crate) fn kicks(&self) -> u64 {
        self.routes.kicks()
    }

    /// Make this Mac's person key where none reads and sign its certificate with the node key
    /// where none stands for it, asking nothing (§11 C3c): a person's post to another Mac is sealed
    /// by it. Done, journaled; failed, journaled once for each reason.
    fn vouch(&self) {
        let now = crate::relay::wall_ms(&self.mesh.wall);
        let me = &self.mesh.me;
        let vouched = aterm_link::person_key::vouch(&me.dir, me.seed(), now);
        let mut kept = self.kept.lock().unwrap_or_else(PoisonError::into_inner);
        match vouched {
            Ok(aterm_link::person_key::Vouched::Kept) => kept.vouch_said = None,
            Ok(aterm_link::person_key::Vouched::Signed { made }) => {
                kept.vouch_said = None;
                aterm_log::info!(
                    "mesh: the node key signed a certificate of {} person key",
                    if made { "a new" } else { "this Mac's" }
                );
            }
            Err(e) => {
                let why = e.to_string();
                if kept.vouch_said.as_deref() != Some(why.as_str()) {
                    aterm_log::warn!(
                        "mesh: the person key could not be vouched for ({why}); a person's post \
                         to another Mac is refused until it is"
                    );
                    kept.vouch_said = Some(why);
                }
            }
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
    /// Stops the pushes, and takes the hand-off and its reader's ring down: with no push running,
    /// the bridge routes to no other Mac (it fails closed on a missing hand-off) until the next
    /// node service writes one.
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
            remove_handoff(&root);
        }
    }
}

/// Take the hand-off down in `root`, and the reader's ring beside it.
fn remove_handoff(root: &std::path::Path) {
    for file in [
        aterm_link::federate::HANDOFF_FILE,
        aterm_link::federate::OLD_READS_FILE,
    ] {
        let _ = std::fs::remove_file(root.join(file));
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

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use aterm_link::federate::node_of;

    use super::handoff_of;

    fn key(tag: u8) -> [u8; 32] {
        astream_broker::origin::public_key(&[tag; 32])
    }

    /// **THE HAND-OFF NAMES EVERY NODE GONE**: a key the log removed and a key a `Node` record
    /// rotated away are gone; a current member, this Mac, and a removed key some member's key
    /// also names (none can, but the rule holds it) are not. Each rotation names its successor.
    #[test]
    fn the_hand_off_names_each_removed_and_rotated_node_gone_and_no_member() {
        let (me, b, removed, old, new) = (key(1), key(2), key(3), key(4), key(5));
        let standings = crate::Standings {
            current: [me, b, new].into(),
            removed: [removed, b].into(),
            retired: [old].into(),
            rotated: [(old, new)].into(),
        };
        let handoff = handoff_of(node_of(&me), &standings);
        assert_eq!(handoff.me, node_of(&me));
        assert_eq!(
            handoff.members,
            [me, b, new].iter().map(node_of).collect::<BTreeSet<_>>()
        );
        assert_eq!(
            handoff.moved,
            BTreeMap::from([(node_of(&old), node_of(&new))])
        );
        assert_eq!(
            handoff.gone,
            [removed, old].iter().map(node_of).collect::<BTreeSet<_>>(),
            "the removed and the rotated node, and no member"
        );
        let none = handoff_of(node_of(&me), &crate::Standings::default());
        assert!(
            none.gone.is_empty(),
            "a log that took no one out names no one gone"
        );
        assert!(
            none.members.contains(&node_of(&me)),
            "a Mac the log has not seated yet counts in"
        );
    }

    /// **A REMOVED MAC NAMES ITSELF GONE**: once the log takes this Mac out, its own hand-off
    /// counts it no member and names it gone, so its bridge lifts the halts it said, as every
    /// other Mac's does (§4.11, "No halt outlives its origin").
    /// NEGATIVE CONTROL: the same Mac still seated is a member and not gone.
    #[test]
    fn a_removed_mac_names_itself_gone() {
        let (me, b) = (key(1), key(2));
        let removed = crate::Standings {
            current: [b].into(),
            removed: [me].into(),
            ..crate::Standings::default()
        };
        let handoff = handoff_of(node_of(&me), &removed);
        assert!(!handoff.members.contains(&node_of(&me)));
        assert_eq!(handoff.gone, [node_of(&me)].into());
        let seated = crate::Standings {
            current: [me, b].into(),
            ..crate::Standings::default()
        };
        let control = handoff_of(node_of(&me), &seated);
        assert!(control.members.contains(&node_of(&me)), "control");
        assert!(control.gone.is_empty(), "control");
    }
}
