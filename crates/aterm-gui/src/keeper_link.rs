// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WINDOW'S KEEPER LINK — P3 of `docs/DESIGN-pty-keeper-2026-09-26.md`
//! (opt-in: `[keeper] enabled = true`; off by default, and then NOTHING here
//! runs: no socket is dialled, no descriptor duplicated).
//!
//! On, this window keeps the PTY keeper told, through `aterm_keeper`'s
//! `KeeperLink` worker (the main thread makes no socket call; the queue holds
//! DUPLICATES, never descriptor numbers):
//!
//! * REGISTER — a close-on-exec duplicate of every master, fresh or adopted,
//!   at the end of `spawn_session` ([`register`]);
//! * META — each registered terminal's Repaint-rung scalar state
//!   (`seamless::keeper_meta`: no grids, no nonce), sent when its fingerprint
//!   changes, at most every [`META_POLL`]; DEC 2026 and `?25` are masked out of
//!   the fingerprint (§11 M9: they churn per frame);
//! * RELEASE — from `Session::drop` ([`release`]);
//! * PENDING — the update successor's pid, in both seamless-update lanes,
//!   written before the grant ([`pending_before_grant`]);
//! * BYE — once, on the final-exit path just before `exit(0)` ([`bye`]; the one
//!   call site is pinned by `the_bye_has_one_call_site`).
//!
//! AT BOOT a window that is not an update's candidate says HELLO, and every
//! OFFER — a crashed window's master the keeper kept — passes the ONE admission
//! ([`admit_recovered_master`]: `seamless::admit_master_descriptor`, the
//! per-record refusals `take_incoming` makes, plus the shell's liveness and
//! birth) into the same `seamless_adopt` landing the update uses, with the dead
//! window's crash journal as the layout. An update's CANDIDATE registers
//! nothing until its Commit ([`activate`], beside
//! `crash_signal::adopt_app_identity`): before it, the outgoing window owns the
//! sessions (`PtyKeeperCustody`'s `RegisterA` guard).
//!
//! P3 never relaunches: `aterm keeper start` runs the keeper with
//! `--no-relaunch`, and the next launch a person makes recovers.

#[cfg(unix)]
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(unix)]
use std::sync::{Arc, Mutex, Weak};
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use aterm_core::terminal::Terminal;
#[cfg(unix)]
use aterm_keeper::client::{KeeperClient, KeeperLink, LinkOp, Offered};
#[cfg(unix)]
use aterm_keeper::wire::{Birth, Frame, MarkerRef, MasterHeader};

/// Whether this process takes part at all: the fast path every call site
/// reads before anything else, so a window with the keeper off pays one
/// relaxed load per spawn, close and exit.
static ON: AtomicBool = AtomicBool::new(false);

/// How often the META pump looks at the registered terminals: the design's
/// 250 ms debounce (§5.3 step 2) — at most four METAs a second per terminal,
/// and none while its modes hold still.
#[cfg(unix)]
pub(crate) const META_POLL: Duration = Duration::from_millis(250);

/// How long the update worker waits for its PENDING to be written before it
/// grants anyway (the holder scan is the keeper's independent gate, §5.4).
#[cfg(unix)]
const PENDING_WAIT: Duration = Duration::from_secs(1);

/// How long the final exit waits for its BYE to be written.
#[cfg(unix)]
const BYE_WAIT: Duration = Duration::from_millis(500);

/// Whether the keeper link is on in this process.
pub(crate) fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

/// The fingerprint whose change sends a META: the modes with the per-frame
/// ones masked (DEC 2026 synchronized output and `?25` cursor visibility —
/// M9: a steady TUI toggles both every frame), the kitty keyboard flags and
/// the xterm keyboard state.
#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    modes: aterm_core::terminal::TerminalModes,
    kitty: aterm_core::terminal::KittyKeyboardFlags,
    xterm: aterm_core::terminal::XtermKeyboardState,
}

#[cfg(unix)]
impl Fingerprint {
    pub(crate) fn of(term: &Terminal) -> Self {
        let mut modes = *term.modes();
        modes.synchronized_output = false;
        modes.cursor_visible = true;
        Self {
            modes,
            kitty: term.kitty_keyboard_flags(),
            xterm: *term.xterm_keyboard(),
        }
    }
}

#[cfg(unix)]
enum Phase {
    /// Not taking part (the default, and `[keeper] enabled = false`).
    Off,
    /// An update's candidate before its Commit: registrations wait here.
    Armed,
    /// Talking to the keeper. Shared, so a waited op (PENDING, BYE) is
    /// waited for with the state lock RELEASED: the main thread's register
    /// and release never queue behind an update worker's bounded wait.
    Live(Arc<KeeperLink>),
}

#[cfg(unix)]
#[derive(Clone)]
struct Tracked {
    rdev: u64,
    term: Weak<Mutex<Terminal>>,
    last: Option<Fingerprint>,
}

#[cfg(unix)]
struct State {
    phase: Phase,
    /// A candidate's registrations, sent at its Commit.
    buffered: Vec<(u64, LinkOp)>,
    /// Every registered session, by this process's session id.
    tracked: BTreeMap<u64, Tracked>,
    pump: bool,
}

#[cfg(unix)]
static KEEPER_STATE: Mutex<State> = Mutex::new(State {
    phase: Phase::Off,
    buffered: Vec::new(),
    tracked: BTreeMap::new(),
    pump: false,
});

#[cfg(unix)]
fn keeper_state() -> std::sync::MutexGuard<'static, State> {
    KEEPER_STATE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The live link, cloned out of the state lock (`None` unless live).
#[cfg(unix)]
fn live_link() -> Option<Arc<KeeperLink>> {
    match &keeper_state().phase {
        Phase::Live(link) => Some(Arc::clone(link)),
        Phase::Armed | Phase::Off => None,
    }
}

/// THE RELEASE OBLIGATION of a recovered master: the keeper still holds its
/// own copy of every master it OFFERED this window (`Offered { to: conn }`
/// until a claim), so an admitted offer that never becomes a registered
/// session must be RELEASED, or the keeper keeps the master open — no hangup
/// reaches the shell, which runs on unseen — and, when this window ends, turns
/// the record back into an orphan the next launch is offered again, even after
/// a clean quit (`KeeperCore::peer_died`). Every keeper-sourced
/// [`crate::spawn::Adopted`] carries one: [`register`] discharges it once the
/// session it became is registered (its RELEASE is then `Session::drop`'s),
/// and dropped undischarged — the orphan net with no window to land in, a
/// spawn that failed — it sends the RELEASE itself, on the link that took the
/// offer (`KeeperLink::start_marked` keeps the boot connection).
///
/// It also says whose the shell was: `owner`, the window that registered it
/// (the tag's), which [`place_by_owner`] matches against the writer of the
/// layout a recovery rebuilds.
#[derive(Debug)]
pub(crate) struct Claim {
    rdev: u64,
    owner: Option<u32>,
    armed: bool,
}

impl Claim {
    /// The obligation for the offered master `rdev`, registered by `owner`.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn new(rdev: u64, owner: Option<u32>) -> Self {
        Self {
            rdev,
            owner,
            armed: true,
        }
    }

    /// The window that registered the master (the tag's `owner`).
    pub(crate) fn owner(&self) -> Option<u32> {
        self.owner
    }

    /// The offered master's device (a test names a claim by it).
    #[cfg(test)]
    pub(crate) fn rdev(&self) -> u64 {
        self.rdev
    }

    /// The master became a registered session: nothing to release here.
    fn discharge(mut self) {
        self.armed = false;
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.armed {
            release_unclaimed(self.rdev);
        }
    }
}

/// The RELEASEs undischarged [`Claim`]s sent, in order (a test's view of the
/// obligation, whether or not a link carries them).
#[cfg(test)]
static UNCLAIMED_RELEASES: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());

/// Whether an undischarged [`Claim`] released `rdev`.
#[cfg(test)]
pub(crate) fn released_unclaimed(rdev: u64) -> bool {
    UNCLAIMED_RELEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(&rdev)
}

/// Release offered master `rdev` that became no session: the keeper closes
/// its copy, and with this window's already closed the shell is hung up.
fn release_unclaimed(rdev: u64) {
    #[cfg(test)]
    UNCLAIMED_RELEASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(rdev);
    if !on() {
        return;
    }
    #[cfg(unix)]
    if let Some(link) = live_link() {
        aterm_log::info!("keeper: an offered master became no session; released");
        let _ = link.enqueue(LinkOp::Release { rdev });
    }
    #[cfg(not(unix))]
    let _ = rdev;
}

/// WHOSE SHELL FILLS WHICH PANE (the recovery's matching). Every window mints
/// its pool ids from 0, so a recovered shell's `local_id` names a leaf only in
/// the layout ITS window wrote: two windows that both died leave offers
/// `{0, 1}` and `{0, 1}`, and a match on the id alone put one window's shell
/// in the other's pane — the wrong folder, title and identity, and the agent
/// that pane hosted not relaunched because a live shell "filled" it.
///
/// So the ids are put in ONE namespace keyed by (writer, id): every terminal
/// leaf of `layout` window `i` by `writers[i]` (the pid of the crash journal it
/// came from; `None` for a window whose writer is unknown — a quit's
/// `session.toml`, which names none), and every keeper-sourced shell by its
/// [`Claim::owner`]. A shell whose (owner, id) no leaf carries takes an id no
/// leaf names, and the orphan net places it as a bare tab. The existing
/// id-matching (`take_handed_off_shell`, the orphan net's identity, the
/// agent's "filled by a live shell") then matches only a window's own shells.
/// A shell with no owner, or one not from the keeper, matches nothing.
pub(crate) fn place_by_owner(
    shells: &mut [crate::spawn::Adopted],
    layout: Option<&mut crate::restore::RestoreManifest>,
    writers: &[Option<u32>],
) {
    use crate::restore::{PaneLayout, RestoredSplitTree, RestoredView};
    let mut ids = std::collections::BTreeMap::<(Option<u32>, u64), u64>::new();
    let mut next = 0u64;
    let mut key = |writer: Option<u32>, id: u64| -> u64 {
        *ids.entry((writer, id)).or_insert_with(|| {
            next += 1;
            next - 1
        })
    };
    fn tree(node: &mut RestoredSplitTree, map: &mut dyn FnMut(u64) -> u64) {
        match node {
            RestoredSplitTree::Leaf {
                view: RestoredView::Terminal(terminal),
            } => terminal.local_id = terminal.local_id.map(&mut *map),
            RestoredSplitTree::Leaf { .. } => {}
            RestoredSplitTree::Split { first, second, .. } => {
                tree(first, map);
                tree(second, map);
            }
        }
    }
    fn legacy(node: &mut PaneLayout, map: &mut dyn FnMut(u64) -> u64) {
        match node {
            PaneLayout::Leaf { local_id, .. } => *local_id = local_id.map(&mut *map),
            PaneLayout::Split { first, second, .. } => {
                legacy(first, map);
                legacy(second, map);
            }
        }
    }
    if let Some(layout) = layout {
        for (i, window) in layout.windows.iter_mut().enumerate() {
            let writer = writers.get(i).copied().flatten();
            let mut map = |id| key(writer, id);
            for tab in &mut window.restored_tabs {
                tree(&mut tab.root, &mut map);
            }
            for tab in &mut window.tabs {
                legacy(tab, &mut map);
            }
        }
    }
    // Ids no leaf carries, above every one a leaf does.
    let mut unnamed = u64::try_from(ids.len()).unwrap_or(u64::MAX);
    for shell in shells {
        let owned = shell.keeper.as_ref().and_then(Claim::owner);
        shell.local_id = match owned.and_then(|owner| ids.get(&(Some(owner), shell.local_id))) {
            Some(&id) => id,
            None => {
                unnamed += 1;
                unnamed - 1
            }
        };
    }
}

/// What the boot HELLO recovered.
#[derive(Default)]
pub(crate) struct Recovery {
    /// The admitted offers, each an adoption for `seamless_adopt`.
    pub(crate) adopted: Vec<crate::spawn::Adopted>,
    /// Offers refused by the admission (each released back to the keeper,
    /// which closes its copy).
    pub(crate) refused: Vec<(u64, &'static str)>,
    /// The dead windows the offers came from (their pids, from the tags).
    pub(crate) owners: Vec<u32>,
    /// The shell pids of the adopted sessions (what `End sessions` ends).
    pub(crate) shells: Vec<i32>,
}

/// The build string a HELLO carries.
#[cfg(unix)]
fn build() -> String {
    format!(
        "{}+{}",
        aterm_types::version::APP_VERSION,
        crate::build_info::BUILD_NUMBER
    )
}

/// This window's crash marker, as the keeper is told it at HELLO (§5.4 row 3's
/// cross-check): the keeper keeps it only if it finds the marker there with its
/// lock held, and after this window's death reads its absence — the exit path
/// unlinked it — together with `exit(0)` as a quit whose BYE was lost.
#[cfg(unix)]
fn marker() -> Option<MarkerRef> {
    use std::os::unix::ffi::OsStrExt as _;
    let (dir, nanos) = crate::crash_signal::own_marker_place()?;
    Some(MarkerRef {
        nanos: u64::try_from(nanos).ok()?,
        dir: dir.as_os_str().as_bytes().to_vec(),
    })
}

/// Whether a launch is an update's CANDIDATE for its keeper — it registers at
/// its Commit and takes no offers: its overlap pair was ADMITTED
/// (`overlap_admitted`, a Commit can come), or it adopted a handoff. The
/// channel variables alone do not make one: a launch whose pair was refused
/// with nothing offered boots fresh as a plain window
/// (`seamless::overlap_intake_exits`), and armed it would stay armed for
/// life — no boot HELLO, no crashed window's shells taken, every
/// registration only buffered, its PENDING and BYE unsent.
pub(crate) fn is_update_candidate(overlap_admitted: bool, adopted_a_handoff: bool) -> bool {
    overlap_admitted || adopted_a_handoff
}

/// BOOT (`main_entry`, beside `take_incoming`): arm the link when the keeper
/// is enabled, and — unless this launch is an update's `candidate`, which
/// waits for its Commit — say the boot HELLO and admit every offer. Returns
/// the recovery (empty when off, when no keeper answers, or when nothing was
/// on offer).
/// Whether a launch takes part in its keeper: every windowed launch, and a
/// headless one only of a build whose keeper is not the installed app's
/// ([`boot`]'s App-class rule).
#[cfg_attr(not(unix), allow(dead_code))]
fn takes_part(headless: bool, installed_keeper: bool) -> bool {
    !(headless && installed_keeper)
}

pub(crate) fn boot(enabled: bool, candidate: bool, headless: bool) -> Recovery {
    if !enabled {
        return Recovery::default();
    }
    // App-class only (§0 decision 6, §5.1): a HEADLESS launch of the
    // installed app never joins the installed app's keeper — neither
    // registering nor taking a crashed window's orphans at its HELLO, which
    // would move a person's recovered tabs into an instance with no window
    // (the P3 review's finding). A dev or test build has its own keeper
    // label (`aterm_keeper::job::place_for`), so its headless instances —
    // the live end-to-end tests' — still take part.
    #[cfg(unix)]
    if !takes_part(headless, aterm_keeper::job::own_place().installed) {
        aterm_log::info!("keeper: a headless launch of the installed app takes no part");
        return Recovery::default();
    }
    let _ = headless;
    #[cfg(unix)]
    {
        boot_unix(candidate)
    }
    #[cfg(not(unix))]
    {
        let _ = candidate;
        aterm_log::info!("keeper: not available on this platform");
        Recovery::default()
    }
}

#[cfg(unix)]
fn place() -> Option<(std::path::PathBuf, aterm_keeper::identity::Identity)> {
    let socket = aterm_keeper::job::socket_path(&aterm_keeper::job::own_place())?;
    match aterm_keeper::identity::Identity::for_self(
        aterm_keeper::identity::IdentityPolicy::Designated,
    ) {
        Ok(identity) => Some((socket, identity)),
        Err(why) => {
            // Fail closed: a window that cannot say who it is cannot check
            // that the keeper is this build's (§5.7), and a same-uid process
            // that bound the socket would receive every terminal.
            aterm_log::warn!("keeper: this build has no code identity to check a keeper by: {why}");
            None
        }
    }
}

#[cfg(unix)]
fn boot_unix(candidate: bool) -> Recovery {
    let Some((socket, identity)) = place() else {
        return Recovery::default();
    };
    ON.store(true, Ordering::Relaxed);
    if candidate {
        keeper_state().phase = Phase::Armed;
        aterm_log::info!("keeper: armed; this update candidate registers at its Commit");
        return Recovery::default();
    }
    let mut recovery = Recovery::default();
    let booted = match KeeperClient::connect(&socket, &identity, Duration::from_secs(2)) {
        Ok(client) => match client.hello_app_marked(&build(), marker()) {
            Ok(offers) => {
                let mut used_cells = 0u64;
                for offer in offers {
                    let rdev = offer.header.rdev;
                    let local_id = offer.header.local_id;
                    if let Some(owner) = Tag::decode(&offer.tag).and_then(|t| t.owner)
                        && !recovery.owners.contains(&owner)
                    {
                        recovery.owners.push(owner);
                    }
                    match admit_recovered_master(offer, &mut used_cells) {
                        Ok(adopted) => {
                            recovery.shells.push(adopted.pid);
                            recovery.adopted.push(adopted);
                        }
                        Err(why) => {
                            aterm_log::warn!(
                                "keeper: refused the offer of session {local_id}: {why}; released"
                            );
                            // The keeper closes its copy: nothing will read it.
                            let _ = client.send(&Frame::Release { rdev }, None);
                            recovery.refused.push((local_id, why));
                        }
                    }
                }
                Some(client)
            }
            Err(e) => {
                aterm_log::warn!("keeper: the boot HELLO failed: {e}");
                None
            }
        },
        Err(e) => {
            aterm_log::info!(
                "keeper=absent at {} ({e}); terminals are registered once one answers",
                socket.display()
            );
            None
        }
    };
    if !recovery.adopted.is_empty() {
        aterm_log::info!(
            "keeper: {} session(s) the last window's end left running are reattached (from pid(s) {:?})",
            recovery.adopted.len(),
            recovery.owners
        );
    }
    match KeeperLink::start_marked(socket, identity, build(), marker(), booted) {
        Ok(link) => {
            keeper_state().phase = Phase::Live(Arc::new(link));
            start_meta_pump();
        }
        Err(e) => {
            aterm_log::warn!("keeper: the link worker did not start: {e}");
            ON.store(false, Ordering::Relaxed);
        }
    }
    recovery
}

/// COMMIT (an update's successor, beside `crash_signal::adopt_app_identity`):
/// from here this process is the window, so the link starts and every
/// registration its adopted sessions queued is sent — the successor's claim
/// that turns the keeper's hold into custody (§5.3 step 8).
pub(crate) fn activate() {
    #[cfg(unix)]
    {
        if !on() {
            return;
        }
        let mut st = keeper_state();
        if !matches!(st.phase, Phase::Armed) {
            return;
        }
        let Some((socket, identity)) = place() else {
            return;
        };
        match KeeperLink::start_marked(socket, identity, build(), marker(), None) {
            Ok(link) => {
                for (_, op) in st.buffered.drain(..) {
                    let _ = link.enqueue(op);
                }
                st.phase = Phase::Live(Arc::new(link));
                drop(st);
                start_meta_pump();
                aterm_log::info!("keeper: this successor's sessions are registered at its Commit");
            }
            Err(e) => aterm_log::warn!("keeper: the link worker did not start: {e}"),
        }
    }
}

/// The opaque tag a registration carries: what the NEXT window needs to adopt
/// the session as itself. The keeper never decodes it (§5.2).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Tag {
    pub(crate) sid: Option<String>,
    /// The window that registered it: the journal a recovery reads is its.
    pub(crate) owner: Option<u32>,
    pub(crate) identity: Option<String>,
    pub(crate) rekey: bool,
    pub(crate) loader: bool,
    pub(crate) frozen: bool,
}

impl Tag {
    /// `key=value` lines, version first; a value with a newline is dropped.
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut out = String::from("v=1\n");
        let mut put = |k: &str, v: &str| {
            if !v.contains('\n') {
                out.push_str(k);
                out.push('=');
                out.push_str(v);
                out.push('\n');
            }
        };
        if let Some(sid) = &self.sid {
            put("sid", sid);
        }
        if let Some(owner) = self.owner {
            put("owner", &owner.to_string());
        }
        if let Some(identity) = &self.identity {
            put("identity", identity);
        }
        for (k, on) in [
            ("rekey", self.rekey),
            ("loader", self.loader),
            ("frozen", self.frozen),
        ] {
            if on {
                put(k, "1");
            }
        }
        out.into_bytes()
    }

    /// The inverse of [`Self::encode`]; `None` for a tag of another version
    /// or one that is not UTF-8. Unknown keys are ignored (frames only grow).
    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let mut lines = text.lines();
        if lines.next()? != "v=1" {
            return None;
        }
        let mut tag = Tag::default();
        for line in lines {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k {
                "sid" if valid_sid(v) => tag.sid = Some(v.to_string()),
                "owner" => tag.owner = v.parse().ok(),
                "identity" if valid_label(v) => tag.identity = Some(v.to_string()),
                "rekey" => tag.rekey = v == "1",
                "loader" => tag.loader = v == "1",
                "frozen" => tag.frozen = v == "1",
                _ => {}
            }
        }
        Some(tag)
    }
}

/// `s-<hex>`, at most 64 characters: the shape every sid this build mints.
fn valid_sid(s: &str) -> bool {
    s.len() <= 64
        && s.strip_prefix("s-")
            .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// An identity label: `[A-Za-z0-9._-]`, 1–64 characters.
fn valid_label(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A process's start time in the keeper wire's shape, from the pty crate's
/// microseconds.
#[cfg(unix)]
fn birth_of(micros: u64) -> Birth {
    Birth {
        seconds: micros / 1_000_000,
        micros: u32::try_from(micros % 1_000_000).unwrap_or(0),
    }
}

/// THE ONE ADMISSION of a keeper OFFER (design §4 item 5, §5.3 step 7): the
/// per-record hard refusals `take_incoming` makes
/// (`seamless::admit_master_descriptor`: a tty, `FD_CLOEXEC` re-armed), then
/// the shell — still alive, still born when it was registered, and still the
/// session leader of THIS terminal (`aterm_pty::record_adopted_shell`, whose
/// identity the session then keeps, F3) — and the tag's session id. Admitted,
/// it is an [`crate::spawn::Adopted`] on the Repaint rung built from its META
/// (`seamless::recovered_checkpoint`), repainted by the size pulse. Refused,
/// the master closes here, through `HandedMaster`'s drop.
#[cfg(unix)]
pub(crate) fn admit_recovered_master(
    offer: Offered,
    used_cells: &mut u64,
) -> Result<crate::spawn::Adopted, &'static str> {
    use std::os::fd::IntoRawFd as _;
    let Offered {
        master,
        header,
        tag,
        meta,
    } = offer;
    let master = crate::spawn::HandedMaster::new(master.into_raw_fd());
    admit_recovered(master, header, &tag, &meta, used_cells)
}

#[cfg(unix)]
fn admit_recovered(
    master: crate::spawn::HandedMaster,
    header: MasterHeader,
    tag: &[u8],
    meta: &[u8],
    used_cells: &mut u64,
) -> Result<crate::spawn::Adopted, &'static str> {
    let fd = master.raw();
    crate::seamless::admit_master_descriptor(fd)?;
    let pid = i32::try_from(header.shell_pid).map_err(|_| "names no shell")?;
    let shell = aterm_pty::record_adopted_shell(pid, fd);
    match shell.birth.map(birth_of) {
        None => return Err("its shell is gone or no longer leads its terminal"),
        Some(birth) if birth != header.shell_birth => {
            return Err("its shell's pid now names another process");
        }
        Some(_) => {}
    }
    let tag = Tag::decode(tag).ok_or("carries a tag this build does not read")?;
    let sid = tag.sid.clone().ok_or("carries no session id")?;
    Ok(crate::spawn::Adopted {
        // From here the master is released if it becomes no session.
        keeper: Some(Claim::new(header.rdev, tag.owner)),
        local_id: header.local_id,
        master,
        pid,
        sid: aterm_session::SessionId::new(sid),
        nonce: aterm_session::LaunchNonce::generate(),
        checkpoint: crate::seamless::recovered_checkpoint(meta, used_cells),
        control: None,
        // The screen is blank on purpose: the pulse makes the program redraw.
        repaint: true,
        frozen_path: tag.frozen,
        identity: tag.identity,
        topics: Vec::new(),
        fg_holder: 0,
        rekey: tag.rekey,
        loader: tag.loader,
        history: crate::handoff_history::AdoptedHistory::default(),
        hold: None,
        title: String::new(),
        supervisor: None,
        attention_owners: Vec::new(),
        claim_grace: false,
        outgoing_build: None,
    })
}

/// REGISTER (the end of `spawn_session`, for a fresh or an adopted master): a
/// close-on-exec duplicate of `session`'s master, its rdev read off that
/// duplicate, the shell's pid and recorded birth, this process's session id as
/// the journal's leaf id, and the tag. A candidate holds it until its Commit.
///
/// `claim` is a recovered master's release obligation: discharged once the
/// session is registered (its RELEASE is then `Session::drop`'s), and left to
/// release the master when the session could not be — the keeper would
/// otherwise keep a copy nothing here will ever release.
pub(crate) fn register(session: &crate::Session, claim: Option<Claim>) {
    if !on() {
        // No link: nothing was offered, and nothing can be released.
        if let Some(claim) = claim {
            claim.discharge();
        }
        return;
    }
    #[cfg(unix)]
    let tracked = register_unix(session);
    #[cfg(not(unix))]
    let tracked = {
        let _ = session;
        false
    };
    if tracked && let Some(claim) = claim {
        claim.discharge();
    }
}

/// Whether the session is now tracked (and so released at its drop).
#[cfg(unix)]
fn register_unix(session: &crate::Session) -> bool {
    use std::os::fd::{BorrowedFd, OwnedFd};
    use std::os::unix::fs::MetadataExt as _;
    if session.master < 3 || session.pid <= 1 {
        return false;
    }
    let Some(shell_birth) = session.shell_identity.birth else {
        // Nothing proves the shell is the one on this terminal: a keeper could
        // never check it, and a recovery would refuse it.
        return false;
    };
    // SAFETY: `session.master` is this live session's open master (its sink
    // owns it for as long as the session exists, and the session is borrowed
    // here); the borrow ends with this statement, which only duplicates it
    // (`F_DUPFD_CLOEXEC`).
    let dup = unsafe { BorrowedFd::borrow_raw(session.master) }.try_clone_to_owned();
    let Ok(dup) = dup else {
        return false;
    };
    let file = std::fs::File::from(dup);
    let Ok(rdev) = file.metadata().map(|m| m.rdev()) else {
        return false;
    };
    let master = OwnedFd::from(file);
    let Ok(shell_pid) = u32::try_from(session.pid) else {
        return false;
    };
    let local_id = session.id;
    let tag = Tag {
        sid: Some(session.ctx.self_id.as_str().to_string()),
        owner: Some(std::process::id()),
        identity: session.identity.clone().filter(|l| valid_label(l)),
        rekey: session.rekey_channel,
        loader: session.body_loader,
        frozen: session.frozen_path,
    };
    let op = LinkOp::Register {
        master,
        header: MasterHeader {
            rdev,
            shell_pid,
            shell_birth: birth_of(shell_birth),
            local_id,
        },
        tag: tag.encode(),
    };
    let mut st = keeper_state();
    st.tracked.insert(
        session.id,
        Tracked {
            rdev,
            term: Arc::downgrade(&session.term),
            last: None,
        },
    );
    match &st.phase {
        Phase::Live(link) => {
            let _ = link.enqueue(op);
        }
        Phase::Armed => st.buffered.push((session.id, op)),
        Phase::Off => {}
    }
    true
}

/// RELEASE (`Session::drop`): the tab or pane closed; the keeper closes its
/// copy once no other window claims the master (`KeeperCore::release`).
pub(crate) fn release(session_id: u64) {
    if !on() {
        return;
    }
    #[cfg(unix)]
    {
        let mut st = keeper_state();
        let Some(tracked) = st.tracked.remove(&session_id) else {
            return;
        };
        st.buffered.retain(|(id, _)| *id != session_id);
        if let Phase::Live(link) = &st.phase {
            let _ = link.enqueue(LinkOp::Release { rdev: tracked.rdev });
        }
    }
    #[cfg(not(unix))]
    let _ = session_id;
}

/// PENDING (both seamless-update lanes, before the grant): name the
/// successor's kernel pid to the keeper, and wait — bounded — for the frame to
/// be written, so the keeper reads the successor's hold before it can hold a
/// master (§5.3 step 8). `true` when written, or when the keeper is off; a
/// `false` is logged and the grant goes on: the holder scan is the keeper's
/// independent gate against a second reader.
pub(crate) fn pending_before_grant(successor: u32) -> bool {
    if !on() {
        return true;
    }
    #[cfg(unix)]
    {
        let written = live_link().is_none_or(|link| {
            link.send_and_wait(LinkOp::Pending { pid: successor }, PENDING_WAIT)
        });
        if !written {
            aterm_log::warn!(
                "keeper: PENDING for successor {successor} was not written; the holder scan \
                 stands alone"
            );
        }
        written
    }
    #[cfg(not(unix))]
    {
        let _ = successor;
        true
    }
}

/// BYE — ONLY from the final-exit path, just before `exit(0)` (§5.3 step 4;
/// `the_bye_has_one_call_site`): quit intent, so the keeper closes this
/// window's masters and the kernel hangs the shells up as today. Waits at most
/// [`BYE_WAIT`] for the frame to be written.
pub(crate) fn bye() {
    if !on() {
        return;
    }
    #[cfg(unix)]
    {
        if let Some(link) = live_link()
            && !link.send_and_wait(LinkOp::Bye, BYE_WAIT)
        {
            aterm_log::warn!(
                "keeper: the BYE was not written; the keeper will judge this quit a crash"
            );
        }
    }
}

/// The META pump: one thread, started with the live link, that looks at every
/// registered terminal every [`META_POLL`] and sends its META when the
/// fingerprint moved. `try_lock` only: a busy terminal is looked at next time,
/// never waited for.
#[cfg(unix)]
fn start_meta_pump() {
    {
        let mut st = keeper_state();
        if st.pump {
            return;
        }
        st.pump = true;
    }
    let spawned = std::thread::Builder::new()
        .name("aterm-keeper-meta".to_string())
        .spawn(|| {
            // Housekeeping: four looks a second at a few scalars, never in the
            // band the program being typed into runs in.
            crate::qos::set_self(crate::qos::Role::Housekeeping);
            loop {
                std::thread::sleep(META_POLL);
                if !on() {
                    return;
                }
                meta_pass();
            }
        });
    if let Err(e) = spawned {
        aterm_log::warn!("keeper: the META pump did not start: {e}");
        keeper_state().pump = false;
    }
}

/// One pass of the pump (and, for tests, one step of it).
#[cfg(unix)]
pub(crate) fn meta_pass() -> usize {
    let targets: Vec<(u64, Tracked)> = keeper_state()
        .tracked
        .iter()
        .map(|(id, t)| (*id, t.clone()))
        .collect();
    let mut sent = 0;
    for (id, Tracked { rdev, term, last }) in targets {
        let Some(term) = term.upgrade() else {
            continue;
        };
        let (fingerprint, meta) = {
            let Ok(engine) = term.try_lock() else {
                continue;
            };
            let fingerprint = Fingerprint::of(&engine);
            if last == Some(fingerprint) {
                continue;
            }
            (fingerprint, crate::seamless::keeper_meta(&engine))
        };
        let mut st = keeper_state();
        let Some(tracked) = st.tracked.get_mut(&id) else {
            continue;
        };
        tracked.last = Some(fingerprint);
        if let (Some(meta), Phase::Live(link)) = (meta, &st.phase)
            && link.enqueue(LinkOp::Meta { rdev, meta })
        {
            sent += 1;
        }
    }
    sent
}

impl crate::App {
    /// `End sessions` (the recovery row): hang up every shell this launch
    /// reattached from the keeper that is still one of its sessions — the
    /// verified-identity hang-up a tab close sends (F3) — so each ends as a
    /// shell that exits does, its tab closing and its master released. The
    /// count hung up.
    pub(crate) fn end_recovered_sessions(&mut self) -> usize {
        let shells = std::mem::take(&mut self.keeper_recovered);
        let mut ended = 0;
        for pooled in self.pool.sessions.values() {
            let session = &pooled.session;
            if shells.contains(&session.pid)
                && session.handoff_local_id.is_some()
                && !session
                    .child_reaped
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                let _ = aterm_pty::hangup_shell(&session.shell_identity, session.master);
                ended += 1;
            }
        }
        ended
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tag_round_trips_and_refuses_what_it_cannot_trust() {
        let tag = Tag {
            sid: Some("s-00ff".into()),
            owner: Some(4242),
            identity: Some("worker".into()),
            rekey: true,
            loader: false,
            frozen: true,
        };
        assert_eq!(Tag::decode(&tag.encode()), Some(tag));
        assert_eq!(Tag::decode(b"v=2\nsid=s-00\n"), None, "another version");
        let hostile =
            Tag::decode(b"v=1\nsid=../../x\nidentity=a b\nowner=zz\nnew=1\n").expect("v1");
        assert_eq!(
            hostile,
            Tag::default(),
            "a sid, label or owner that is not one is dropped; unknown keys ignored"
        );
        assert!(tag_under_the_wire_bound());
    }

    fn tag_under_the_wire_bound() -> bool {
        let long = Tag {
            sid: Some(format!("s-{}", "f".repeat(62))),
            owner: Some(u32::MAX),
            identity: Some("x".repeat(64)),
            rekey: true,
            loader: true,
            frozen: true,
        };
        let bytes = long.encode();
        Tag::decode(&bytes).is_some_and(|t| t == long) && bytes.len() <= aterm_keeper::wire::MAX_TAG
    }

    /// APP-CLASS ONLY (§0 decision 6): a headless launch of the installed app
    /// never joins the installed app's keeper — it would take a crashed
    /// window's tabs into an instance with no window. A windowed launch does,
    /// and so does a dev or test build's headless instance, whose keeper is
    /// its own (the live end-to-end tests').
    #[test]
    fn a_headless_launch_of_the_installed_app_takes_no_part() {
        assert!(!takes_part(true, true));
        assert!(takes_part(false, true));
        assert!(takes_part(true, false));
        assert!(takes_part(false, false));
    }

    /// OFF means off: with `[keeper] enabled = false` the boot does not dial,
    /// and a spawn's registration, a close's release, the update's PENDING
    /// and the exit's BYE do nothing (the flag is never raised).
    #[test]
    fn with_the_keeper_off_nothing_registers() {
        let recovery = boot(false, false, false);
        assert!(recovery.adopted.is_empty());
        assert!(!on(), "boot(false) leaves the link off");
        assert!(
            pending_before_grant(1),
            "an update never waits on an absent keeper"
        );
        release(7);
        bye();
        #[cfg(unix)]
        {
            assert!(keeper_state().tracked.is_empty(), "nothing tracked");
            assert!(matches!(keeper_state().phase, Phase::Off));
        }
    }

    /// The fingerprint masks the per-frame modes (M9): a TUI's DEC 2026 and
    /// `?25` toggles send no META, while a real mode change does.
    #[cfg(unix)]
    #[test]
    fn the_fingerprint_ignores_the_per_frame_modes() {
        let mut t = Terminal::new(24, 80);
        let base = Fingerprint::of(&t);
        t.process(b"\x1b[?2026h\x1b[?25l");
        assert_eq!(
            Fingerprint::of(&t),
            base,
            "sync output and cursor visibility are masked"
        );
        t.process(b"\x1b[?2026l\x1b[?25h\x1b[?2004h");
        assert_ne!(Fingerprint::of(&t), base, "bracketed paste is a real mode");
        let bracketed = Fingerprint::of(&t);
        t.process(b"\x1b[>1u");
        assert_ne!(
            Fingerprint::of(&t),
            bracketed,
            "a kitty keyboard push is a real change"
        );
    }

    /// `[keeper] enabled` is OFF unless the file says `true` (P3 is opt-in).
    #[test]
    fn the_keeper_is_off_unless_the_file_turns_it_on() {
        let parse = |text: &str| crate::app_config::Config::parse(text).expect("parses");
        assert!(!parse("").keeper_enabled(), "absent is off");
        assert!(!parse("[keeper]\n").keeper_enabled());
        assert!(!parse("[keeper]\nenabled = false\n").keeper_enabled());
        assert!(parse("[keeper]\nenabled = true\n").keeper_enabled());
    }

    /// BYE is written from the final-exit path and nowhere else (§5.3 step 4):
    /// a second site would tell the keeper "quit" on a path that does not end
    /// the process, and the keeper would close masters a live window reads.
    /// The idiom of `spawn.rs`'s wiring guards: the source itself is read.
    #[test]
    fn the_bye_has_one_call_site() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sites = Vec::new();
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("read");
                for (n, line) in text.lines().enumerate() {
                    let code = line.split("//").next().unwrap_or("");
                    // Spelled in two halves so this line is not a site itself.
                    if code.contains(&["keeper_link", "::bye()"].concat()) {
                        sites.push((path.clone(), n));
                    }
                }
            }
        }
        assert_eq!(sites.len(), 1, "exactly one BYE site: {sites:?}");
        let (file, line) = &sites[0];
        assert!(file.ends_with("lib.rs"), "{file:?}");
        let text = std::fs::read_to_string(file).expect("read");
        let after: Vec<&str> = text.lines().skip(line + 1).take(3).collect();
        assert!(
            after.iter().any(|l| l.trim() == "std::process::exit(0);"),
            "the BYE sits just before the final exit(0): {after:?}"
        );
    }

    /// THE UPDATE INTERACTION (§5.3 step 8, "ships in P3, never later"): in
    /// each lane the PENDING precedes the grant — the rendezvous transfer of
    /// the masters, and the fork lane's proof/Commit decision — and the
    /// successor's registration is released at its Commit, beside the
    /// identity it adopts there, and nowhere else. Read off the source, as
    /// `spawn.rs`'s wiring guards are.
    #[test]
    fn the_update_lanes_send_pending_before_the_grant_and_register_at_commit() {
        let handoff = include_str!("app_update_handoff.rs");
        let oob_pending = handoff
            .find("keeper_link::pending_before_grant(held.candidate.pid)")
            .expect("the launched lane sends PENDING");
        let transfer = handoff.find(".transfer(").expect("the grant");
        assert!(
            oob_pending < transfer,
            "PENDING before the rendezvous grant"
        );
        let fork_pending = handoff
            .find("keeper_link::pending_before_grant(candidate.pid)")
            .expect("the fork lane sends PENDING");
        let spawn = handoff
            .find("let child = match job.command.spawn()")
            .expect("the fork");
        // The fork lane's OWN decision: the first after its fork. (A search
        // from the PENDING would also find the launched lane's, further down
        // the file, and pass a PENDING moved past this lane's grant.)
        let decision = spawn
            + handoff[spawn..]
                .find("run_handoff_decision(")
                .expect("the fork lane's decision follows its fork");
        assert!(
            spawn < fork_pending && fork_pending < decision,
            "PENDING between the fork and the fork lane's decision: spawn={spawn} \
             pending={fork_pending} decision={decision}"
        );
        assert_eq!(
            handoff
                .matches("keeper_link::pending_before_grant(")
                .count(),
            2,
            "one PENDING per lane"
        );
        let lib = include_str!("lib.rs");
        let adopt = lib
            .find("if crate::crash_signal::adopt_app_identity() {")
            .expect("the Commit's identity adoption");
        let activate = lib
            .find("crate::keeper_link::activate();")
            .expect("the Commit registers");
        assert!(adopt < activate && activate - adopt < 1_000, "beside it");
        assert_eq!(lib.matches("keeper_link::activate()").count(), 1);
    }

    /// P3 REVIEW — A CANDIDATE IS AN ADMITTED PAIR, NOT THE VARIABLES. A
    /// launch whose overlap pair was refused with nothing offered boots fresh
    /// as a plain window; armed as a candidate it would never say its HELLO —
    /// no crashed window's shells taken, every registration only buffered,
    /// crash survival off though `[keeper] enabled = true`. `main_entry`
    /// decides it from the admitted pair (`handoff_commit`), and a shell the
    /// keeper kept, having no parked twin, is read at once even under the
    /// variables. Read off the source, as the update lanes' guard above is.
    #[test]
    fn only_an_admitted_overlap_pair_or_an_adopted_handoff_arms_a_candidate() {
        assert!(
            !is_update_candidate(false, false),
            "a degraded or plain launch"
        );
        assert!(is_update_candidate(true, false), "an admitted pair");
        assert!(is_update_candidate(false, true), "an adopted handoff");
        assert!(is_update_candidate(true, true));

        let code = |text: &'static str| -> String {
            text.lines()
                .map(|line| line.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let lib = code(include_str!("lib.rs"));
        assert!(
            lib.contains("let overlap_admitted = handoff_commit.is_some();"),
            "the candidate is decided by the admitted pair"
        );
        let boot = lib.find("keeper_link::boot(").expect("the boot");
        let args = &lib[boot..boot + 200];
        assert!(
            args.contains("keeper_link::is_update_candidate(overlap_admitted, seamless_adopting)"),
            "{args}"
        );
        assert!(!args.contains("overlap_channels_present"), "{args}");
        // The recovered shells are placed by owner before any pane takes one.
        let place = lib
            .find("keeper_link::place_by_owner(")
            .expect("the placement");
        let session0 = lib
            .find("app_restore::take_session0_shell(&mut seamless_adopt")
            .expect("session 0's pick");
        assert!(boot < place && place < session0);

        let spawn = code(include_str!("spawn.rs"));
        assert!(spawn.contains("let recovered = keeper_claim.is_some();"));
        assert!(
            spawn.contains("if !(adopted && factory.defer_adopted_readers && !recovered) {"),
            "a recovered shell's reader is never deferred to a Commit that will not come"
        );
        assert!(spawn.contains("crate::keeper_link::register(&session, keeper_claim);"));
    }

    /// A real PTY whose shell leads it: `/bin/sh` parked in a `sleep`, and
    /// its recorded birth once `record_adopted_shell` proves it leads the
    /// terminal (it has run `setsid`).
    #[cfg(unix)]
    fn live_pty() -> (aterm_pty::SpawnedShell, u64) {
        // SAFETY: a test process; the trusted-launcher contract trivially holds.
        let authority = unsafe { aterm_cap::Authority::root_authority() };
        let spawn_cap = authority.grant::<aterm_cap::effects::Spawn>(aterm_cap::Tier::Trusted);
        let sandbox_cap = authority.grant::<aterm_sandbox::Sandbox>(aterm_cap::Tier::Trusted);
        let exec: Vec<String> = vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()];
        let sh = aterm_pty::spawn_shell_with_pid_cell_px(
            24,
            80,
            &spawn_cap,
            &sandbox_cap,
            &[],
            None,
            None,
            None,
            Some(&exec),
            None,
            None,
            aterm_sandbox::Limits::inherit(),
            None,
        )
        .expect("spawn");
        // A hang detector, not a latency budget: the child's `setsid` is the event.
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(birth) = aterm_pty::record_adopted_shell(sh.pid, sh.master).birth {
                return (sh, birth);
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the shell never led its pty"
            );
            std::thread::yield_now();
        }
    }

    #[cfg(unix)]
    fn dup_of(fd: i32) -> crate::spawn::HandedMaster {
        use std::os::fd::{BorrowedFd, IntoRawFd as _};
        // SAFETY: `fd` is a live descriptor the test holds for this call.
        let dup = unsafe { BorrowedFd::borrow_raw(fd) }
            .try_clone_to_owned()
            .expect("dup");
        crate::spawn::HandedMaster::new(dup.into_raw_fd())
    }

    #[cfg(unix)]
    fn header_for(pid: i32, birth: u64) -> MasterHeader {
        MasterHeader {
            rdev: 1,
            shell_pid: u32::try_from(pid).expect("pid"),
            shell_birth: birth_of(birth),
            local_id: 3,
        }
    }

    #[cfg(unix)]
    fn tag() -> Vec<u8> {
        Tag {
            sid: Some("s-0123456789abcdef0123".into()),
            owner: Some(1),
            identity: None,
            rekey: true,
            loader: false,
            frozen: false,
        }
        .encode()
    }

    #[cfg(unix)]
    fn end(sh: &aterm_pty::SpawnedShell) {
        // SIGKILL (AGENTS.md rule 6), then reap.
        let identity = aterm_pty::record_spawned_shell(sh.pid);
        let _ = aterm_pty::hangup_shell(&identity, sh.master);
        aterm_pty::reap_shell(identity);
        aterm_pty::close_fd(sh.master);
    }

    /// THE ONE ADMISSION, driven with the three shapes §6.4 names: a master
    /// that is not a tty, a live shell's tty, and a record whose shell is gone
    /// or whose pid names another process. Each refusal closes the offered
    /// descriptor through `HandedMaster`'s drop (the no-leak obligation).
    #[cfg(unix)]
    #[test]
    fn the_admission_takes_a_live_shell_and_refuses_the_rest() {
        let mut used = 0;
        let mut t = Terminal::new(24, 80);
        t.process(b"\x1b[?2004h");
        let meta = crate::seamless::keeper_meta(&t).expect("meta");

        // Not a tty.
        let null = std::fs::File::open("/dev/null").expect("null");
        use std::os::fd::AsRawFd as _;
        let refused = admit_recovered(
            dup_of(null.as_raw_fd()),
            header_for(i32::try_from(std::process::id()).expect("pid"), 0),
            &tag(),
            &meta,
            &mut used,
        );
        assert_eq!(refused.err(), Some("was handed a master that is not a tty"));

        // A live shell leading its tty: admitted, on the Repaint rung with
        // its modes, repainted, and re-keyed through its channel.
        let (sh, birth) = live_pty();
        let adopted = admit_recovered(
            dup_of(sh.master),
            header_for(sh.pid, birth),
            &tag(),
            &meta,
            &mut used,
        )
        .expect("a live shell is admitted");
        assert_eq!(adopted.pid, sh.pid);
        assert_eq!(adopted.local_id, 3);
        assert_eq!(adopted.sid.as_str(), "s-0123456789abcdef0123");
        assert!(adopted.repaint && adopted.rekey);
        assert_eq!(
            adopted
                .keeper
                .as_ref()
                .map(|claim| (claim.rdev(), claim.owner())),
            Some((1, Some(1))),
            "an admitted offer carries its release obligation and its window"
        );
        assert!(
            adopted
                .checkpoint
                .as_ref()
                .is_some_and(|cp| cp.modes.bracketed_paste),
            "the META's modes survive"
        );
        drop(adopted);

        // The pid now names another process (a different birth).
        let refused = admit_recovered(
            dup_of(sh.master),
            header_for(sh.pid, birth + 1),
            &tag(),
            &meta,
            &mut used,
        );
        assert_eq!(
            refused.err(),
            Some("its shell's pid now names another process")
        );

        // No session id in the tag.
        let refused = admit_recovered(
            dup_of(sh.master),
            header_for(sh.pid, birth),
            b"v=1\n",
            &meta,
            &mut used,
        );
        assert_eq!(refused.err(), Some("carries no session id"));

        // The shell is gone.
        let master = dup_of(sh.master);
        end(&sh);
        let refused = admit_recovered(master, header_for(sh.pid, birth), &tag(), &meta, &mut used);
        assert_eq!(
            refused.err(),
            Some("its shell is gone or no longer leads its terminal")
        );
    }

    /// META round-trips through the keeper's bound onto the Repaint rung, the
    /// modes intact and no nonce taken.
    #[cfg(unix)]
    #[test]
    fn a_meta_lands_on_the_repaint_rung_with_its_modes() {
        let mut t = Terminal::new(24, 80);
        t.process(b"\x1b[?2004h\x1b[?1049h\x1b[>1u");
        let meta = crate::seamless::keeper_meta(&t).expect("fits");
        assert!(meta.len() <= aterm_keeper::wire::MAX_META);
        let text = String::from_utf8(meta.clone()).expect("utf-8");
        assert!(!text.contains("\"shell_integration_nonce\""), "{text}");
        let mut used = 0;
        let cp = crate::seamless::recovered_checkpoint(&meta, &mut used).expect("a checkpoint");
        assert!(cp.modes.bracketed_paste, "bracketed paste survives");
        assert!(cp.modes.alternate_screen, "the alt screen survives");
        assert!(cp.shell_integration_nonce.is_none());
        let recovered = Terminal::from_checkpoint(&cp);
        assert_eq!(recovered.kitty_keyboard_flags(), t.kitty_keyboard_flags());
        assert!(
            crate::seamless::recovered_checkpoint(b"{}", &mut used).is_none(),
            "a meta without the handoff's keys is none"
        );
        assert!(crate::seamless::recovered_checkpoint(b"", &mut used).is_none());
    }
}
