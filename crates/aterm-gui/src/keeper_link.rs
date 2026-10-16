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
//!   (`seamless::keeper_meta`: no grids, no nonce, at most 4 KiB at any
//!   width) and its session's fabric (`fabric::KeeperCarry`: the hold, the
//!   topics, an id lease, the queued post ids, the owed receipts' count),
//!   sent when either changes, at most every [`META_POLL`], and tried again
//!   each pass until it is sent; DEC 2026 and `?25` are masked out of the
//!   fingerprint (§11 M9: they churn per frame);
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
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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

/// THE UPDATE'S PARENT (`0`: none), which a candidate's adopted sessions
/// were the parent's sessions of: set once at boot ([`set_predecessor`]), read
/// by every registration ([`registration_tag`]).
#[cfg_attr(not(unix), allow(dead_code))]
static PREDECESSOR: AtomicU32 = AtomicU32::new(0);

/// This launch is an update's candidate whose parent is `pid`: its adopted
/// sessions' registrations name the parent's session each one was
/// ([`Tag::handed`]).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn set_predecessor(pid: u32) {
    PREDECESSOR.store(pid, Ordering::Relaxed);
}

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
    fabric: Weak<crate::fabric::SessionFabric>,
    /// What the last META SENT said: the terminal's fingerprint and the
    /// session's fabric.
    last: Option<(Fingerprint, crate::fabric::KeeperCarry)>,
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
    /// The update's parent's (window, session id) this shell was, when its
    /// registrant adopted it in an update ([`Tag::handed`]).
    handed: Option<(u32, u64)>,
    /// The pair the crash journal image its registrant took over keys it by,
    /// when that image was older than its parent's ([`Tag::keyed`]).
    keyed: Option<(u32, u64)>,
    armed: bool,
}

impl Claim {
    /// The obligation for the offered master `rdev`, registered by `owner`.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn new(rdev: u64, owner: Option<u32>) -> Self {
        Self {
            rdev,
            owner,
            handed: None,
            keyed: None,
            armed: true,
        }
    }

    /// The obligation for the offered master `rdev`, whose registration said
    /// `tag`: its window, and the parent's session it was.
    #[cfg_attr(not(unix), allow(dead_code))]
    pub(crate) fn of_tag(rdev: u64, tag: &Tag) -> Self {
        Self {
            handed: tag.handed,
            keyed: tag.keyed,
            ..Self::new(rdev, tag.owner)
        }
    }

    /// The window that registered the master (the tag's `owner`).
    pub(crate) fn owner(&self) -> Option<u32> {
        self.owner
    }

    /// The update's parent's (window, session id) the shell was
    /// ([`Tag::handed`]).
    pub(crate) fn handed(&self) -> Option<(u32, u64)> {
        self.handed
    }

    /// The pair the crash journal image its registrant took over keys the
    /// shell by ([`Tag::keyed`]).
    pub(crate) fn keyed(&self) -> Option<(u32, u64)> {
        self.keyed
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
///
/// AN UPDATE'S SUCCESSOR RE-TAGS WHAT IT ADOPTED (round seven of the update
/// audit, finding 5's second review). At its Commit the successor registers
/// every adopted shell under ITS window and ITS pool id — a counter that
/// starts over in it — and the keeper keeps only the latest tag. Its crash
/// journal is still the PARENT's image, taken over at Commit, whose leaves
/// carry the parent's ids until the successor's own first image replaces it.
/// So such a registration also names the parent's (window, id) the shell was
/// ([`Claim::handed`]), and a shell whose own (owner, id) no leaf carries takes
/// the leaf that names that pair: a successor that dies between its Commit
/// and its first image gets every pane its own live shell back — the agent's
/// pane included, so the agent is never relaunched beside itself.
///
/// AND ITS OWN IMAGE NAMES WHAT IT WAS HANDED (round eight of the update
/// audit, finding 46). Once the successor has published its own image, its
/// leaves carry its own ids; if its re-registration never reached the keeper
/// (a link that did not start, a crash inside the delivery), the keeper still
/// holds the parent's tag, which names neither. So the image's header names,
/// for each such leaf, the pairs it was handed as
/// (`crash_journal::JournalHeader::handed`, `handed` here: by the image's
/// writer), and a shell held under one of those pairs takes that leaf —
/// never over a leaf whose own pair it is.
///
/// AND THE IMAGE'S OWN PAIR, ANY NUMBER OF UPDATES ON (round seven of the
/// update audit, leftover I2b; finding 209, `docs/DESIGN-pty-keeper-2026-09-26.md`
/// §5.5). `handed` names the registrant's PARENT only. In a chain G → P → S
/// whose image is still G's when S takes it over (P's first image failed to
/// write, and P's desk did not change before it updated again), S's shells
/// named P's pairs, matched none of G's leaves, and went to the orphan net.
/// So P's handoff record says the pair the image it still holds keys each
/// shell by (`SessionRecord::journal_key`, `App::journal_keys`), S's
/// registration names it ([`Claim::keyed`]), and a shell whose own and
/// parent's pairs match no leaf takes the leaf that names that one.
pub(crate) fn place_by_owner(
    shells: &mut [crate::spawn::Adopted],
    layout: Option<&mut crate::restore::RestoreManifest>,
    writers: &[Option<u32>],
    handed: &[(u32, crate::crash_journal::Handed)],
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
    // A leaf's earlier pairs, after every leaf's own: one never takes the key
    // of a leaf whose own pair it is.
    for (writer, pair) in handed {
        if let Some(&leaf) = ids.get(&(Some(*writer), pair.id)) {
            ids.entry((Some(pair.pid), pair.was)).or_insert(leaf);
        }
    }
    for shell in shells {
        let claim = shell.keeper.as_ref();
        let own = claim
            .and_then(Claim::owner)
            .and_then(|owner| ids.get(&(Some(owner), shell.local_id)));
        let was = claim
            .and_then(Claim::handed)
            .and_then(|(owner, id)| ids.get(&(Some(owner), id)));
        let keyed = claim
            .and_then(Claim::keyed)
            .and_then(|(owner, id)| ids.get(&(Some(owner), id)));
        shell.local_id = match own.or(was).or(keyed) {
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
        crate::build_info::version(),
        crate::build_info::build_number()
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
    // Room for what the HELLO hands over, before it does (round seven,
    // finding 26's review): each adopted session holds about four
    // descriptors, and a cold launch runs at launchd's soft 256.
    #[cfg(target_os = "macos")]
    crate::handoff_rendezvous::raise_descriptor_limit_for_recovery();
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
        Err(e) if aterm_keeper::client::refused_identity(&e) => {
            aterm_log::warn!(
                "keeper: the keeper at {} is not this build's; nothing is registered with it",
                socket.display()
            );
            None
        }
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
    /// THE PARENT'S NAME FOR IT (additive, `handed=<pid>:<id>`; a build that
    /// predates it ignores the key): an update's successor that adopted this
    /// shell names the parent window and the parent's pool id it had there,
    /// which the parent's crash journal — taken over at Commit — still keys
    /// its pane by ([`place_by_owner`]).
    pub(crate) handed: Option<(u32, u64)>,
    /// THE IMAGE'S NAME FOR IT (additive, `keyed=<pid>:<id>`; a build that
    /// predates it ignores the key; round seven of the update audit, leftover
    /// I2b): when the parent's journal still held an image OLDER than the
    /// parent's own — the parent took its own parent's over and never
    /// replaced it — the (window, id) that image keys this shell's pane by,
    /// which the parent carried in its handoff record
    /// (`SessionRecord::journal_key`). `handed` alone names only one
    /// generation back, and that image's leaves carry none of its ids.
    pub(crate) keyed: Option<(u32, u64)>,
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
        if let Some((owner, id)) = self.handed {
            put("handed", &format!("{owner}:{id}"));
        }
        if let Some((owner, id)) = self.keyed {
            put("keyed", &format!("{owner}:{id}"));
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
                "handed" => {
                    tag.handed = v
                        .split_once(':')
                        .and_then(|(owner, id)| Some((owner.parse().ok()?, id.parse().ok()?)));
                }
                "keyed" => tag.keyed = crate::session_store::parse_journal_key(v),
                _ => {}
            }
        }
        Some(tag)
    }
}

/// `s-<hex>`, at most 64 characters: the shape every sid this build mints.
pub(crate) fn valid_sid(s: &str) -> bool {
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
    // THE SESSION'S FABRIC, as its last META said it (the fabric audit's
    // fourth round, findings 2-5; `fabric::KeeperCarry`): the sid and its
    // agent survive the crash, so its hold stands again, its broadcast
    // opt-ins resume from the bridge's cursors, its ids count on above the
    // dead window's, and the posts it was told are queued are retired dead
    // `not-carried` — all through `seed_adopted_fabric`, before the session
    // answers a verb.
    let fabric = crate::seamless::recovered_fabric(meta);
    if fabric.topics_lost > 0 || fabric.queued_lost > 0 {
        aterm_log::warn!(
            "keeper: session {sid}'s last META had no room to name {} broadcast opt-in(s) and {} \
             queued post(s); the opt-ins are not restored (`topic add` them again), and whether \
             those posts were sent is not known",
            fabric.topics_lost,
            fabric.queued_lost
        );
    }
    Ok(crate::spawn::Adopted {
        // From here the master is released if it becomes no session.
        keeper: Some(Claim::of_tag(header.rdev, &tag)),
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
        topics: fabric.topics,
        fg_holder: 0,
        rekey: tag.rekey,
        loader: tag.loader,
        history: crate::handoff_history::AdoptedHistory::default(),
        holds: fabric.holds,
        title: String::new(),
        supervisor: None,
        attention_owners: Vec::new(),
        claim_grace: false,
        outgoing_build: None,
        render_debt: None,
        fabric: fabric.carry,
        timeline_id: 0,
        // A crashed window's shell: no manifest carried its log's counters.
        log_carry: None,
        journal_key: None,
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
    let predecessor = Some(PREDECESSOR.load(Ordering::Relaxed)).filter(|&pid| pid != 0);
    let tag = registration_tag(session, std::process::id(), predecessor);
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
            fabric: Arc::downgrade(&session.ctx.fabric),
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

/// What `session`'s registration by window `own_pid` says of it: its sid,
/// its window, its identity and channels — and, for a shell this window
/// adopted in an update from `predecessor`, the parent's (window, id) it was
/// ([`Tag::handed`]).
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn registration_tag(
    session: &crate::Session,
    own_pid: u32,
    predecessor: Option<u32>,
) -> Tag {
    Tag {
        sid: Some(session.ctx.self_id.as_str().to_string()),
        owner: Some(own_pid),
        identity: session.identity.clone().filter(|l| valid_label(l)),
        rekey: session.rekey_channel,
        loader: session.body_loader,
        frozen: session.frozen_path,
        handed: predecessor.zip(session.handoff_local_id),
        keyed: session.journal_key,
    }
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
            aterm_log::warn!("keeper: the BYE was not written");
        }
    }
}

/// The META pump: one thread, started with the live link, that looks at every
/// registered terminal every [`META_POLL`] and attempts its META when the
/// fingerprint or the session's fabric moved. `try_lock` only: a busy
/// terminal is looked at next time, never waited for. Contention, a full
/// queue or a failed link can leave the keeper's last sample older than one
/// polling interval; the interval is not a delivery-age bound.
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

/// One pass of the pump (and, for tests, one step of it): each registered
/// terminal whose fingerprint, or whose session's fabric, differs from what
/// its last SENT META had is sent its META now.
#[cfg(unix)]
pub(crate) fn meta_pass() -> usize {
    meta_pass_over(&KEEPER_STATE, |meta, fabric| {
        crate::seamless::keeper_meta_of(meta, Some(fabric))
    })
}

/// [`meta_pass`] over `state`, building each META with `build` from the
/// terminal's meta carry and the session's fabric. The fabric is read first,
/// under its own leaf locks with no other lock held; only the fingerprint and
/// the carry are taken under the terminal's lock, which the PTY reader waits
/// on; the META is serialized (and shed to fit, up to 404 µs a call at 4,096
/// alternate-screen columns in a debug build) after it is released.
#[cfg(unix)]
fn meta_pass_over<B>(state: &Mutex<State>, build: B) -> usize
where
    B: Fn(aterm_core::terminal::CheckpointMeta, &crate::fabric::KeeperCarry) -> Option<Vec<u8>>,
{
    let targets: Vec<(u64, Tracked)> = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .tracked
        .iter()
        .map(|(id, t)| (*id, t.clone()))
        .collect();
    let mut sent = 0;
    for (
        id,
        Tracked {
            rdev,
            term,
            fabric,
            last,
        },
    ) in targets
    {
        let Some(term) = term.upgrade() else {
            continue;
        };
        let carry = fabric
            .upgrade()
            .map(|fabric| fabric.keeper_carry())
            .unwrap_or_default();
        let (fingerprint, meta) = {
            let Ok(engine) = term.try_lock() else {
                continue;
            };
            let fingerprint = Fingerprint::of(&engine);
            if last
                .as_ref()
                .is_some_and(|(sent, sent_carry)| *sent == fingerprint && *sent_carry == carry)
            {
                continue;
            }
            (fingerprint, engine.carry_meta_abandoning_partial())
        };
        let meta = build(meta, &carry);
        let mut st = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let State { phase, tracked, .. } = &mut *st;
        let Some(tracked) = tracked.get_mut(&id) else {
            continue;
        };
        // Recorded only for a META that went (R17-HANDOFF-1, 2026-10-11): one
        // not built, or that found no link or a full queue, is tried again on
        // the next pass, not when the modes next move.
        if let (Some(meta), Phase::Live(link)) = (meta, &*phase)
            && link.enqueue(LinkOp::Meta { rdev, meta })
        {
            tracked.last = Some((fingerprint, carry));
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
            handed: Some((4141, 7)),
            keyed: Some((4040, 3)),
        };
        assert_eq!(Tag::decode(&tag.encode()), Some(tag));
        assert_eq!(Tag::decode(b"v=2\nsid=s-00\n"), None, "another version");
        let hostile =
            Tag::decode(b"v=1\nsid=../../x\nidentity=a b\nowner=zz\nhanded=1\nkeyed=0:1\nnew=1\n")
                .expect("v1");
        assert_eq!(
            hostile,
            Tag::default(),
            "a sid, label, owner or parent that is not one is dropped; unknown keys ignored"
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
            handed: Some((u32::MAX, u64::MAX)),
            keyed: Some((u32::MAX, u64::MAX)),
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
        // The candidate's registrations name the parent's session (finding 5's
        // second review): the parent is named beside the journal's take-over,
        // at boot, and every registration reads it.
        let lane = lib
            .find("crash_journal.set_predecessor(parent.pid(), parent.birth_unix_nanos());")
            .expect("the journal's predecessor");
        let named = lib
            .find("keeper_link::set_predecessor(parent.pid());")
            .expect("the keeper's predecessor");
        assert!(lane < named && named - lane < 600, "beside it");
        let own = include_str!("keeper_link.rs");
        let register = &own[own.find("fn register_unix(").expect("register_unix")..];
        assert!(
            register[..register.find("\n}\n").expect("its end")]
                .contains("registration_tag(session, std::process::id(), predecessor)"),
            "every registration is tagged by `registration_tag`"
        );
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
            spawn.contains(
                "if !(adopted && factory.defer_adopted_readers && !recovered) && !factory.reader_after_install {"
            ),
            "a recovered shell's reader is never deferred to a Commit that will not come"
        );
        assert!(spawn.contains("crate::keeper_link::register(&session, keeper_claim);"));

        // ROOM FOR THE RECOVERY (round seven, finding 26's review): the soft
        // descriptor limit is raised before the HELLO hands any master over,
        // as a handoff's claim raises it before it dials.
        let own = code(include_str!("keeper_link.rs"));
        let body = &own[own.find("fn boot_unix(").expect("boot_unix")..];
        let raise = body
            .find("crate::handoff_rendezvous::raise_descriptor_limit_for_recovery();")
            .expect("the recovery raises the limit");
        let hello = body.find(".hello_app_marked(").expect("the HELLO");
        assert!(raise < hello, "raised before the HELLO");
        let candidate = body.find("if candidate {").expect("the candidate's return");
        assert!(
            candidate < raise,
            "a candidate takes no offers and raises nothing here"
        );
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
        let exec: Vec<std::ffi::OsString> =
            vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()];
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
            handed: None,
            keyed: None,
        }
        .encode()
    }

    #[cfg(unix)]
    fn end(sh: &aterm_pty::SpawnedShell) {
        // SIGKILL (AGENTS.md), then reap.
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
        // A META without a fabric (an older window's): no hold and no topic,
        // no `dropped=` to go on from, and the counters fenced above any
        // window's, not restarted.
        assert_eq!((adopted.holds.len(), adopted.topics.len()), (0, 0));
        assert_eq!(adopted.fabric.dropped, 0);
        assert!(
            adopted.fabric.last_post_id >= 1 << 32,
            "{:?}",
            adopted.fabric
        );
        drop(adopted);

        // THE SESSION'S FABRIC rides the META (the fabric audit's fourth
        // round, findings 2-5): the recovered session's hold, opt-ins, id
        // lease, queued posts and owed receipts are the dead window's.
        let fabric = crate::fabric::KeeperCarry {
            hold: Some("fleet human%20stop".to_string()),
            more_holds: vec!["local mine".to_string()],
            ids: [8192, 12288, 8192],
            topics: vec!["builds @91234".to_string()],
            queued: vec![4],
            owed: 2,
            dropped: 9,
            ..crate::fabric::KeeperCarry::default()
        };
        let carried =
            crate::seamless::keeper_meta_of(t.carry_meta_abandoning_partial(), Some(&fabric))
                .expect("meta");
        let adopted = admit_recovered(
            dup_of(sh.master),
            header_for(sh.pid, birth),
            &tag(),
            &carried,
            &mut used,
        )
        .expect("a live shell is admitted");
        assert_eq!(adopted.holds, ["fleet human%20stop", "local mine"]);
        assert_eq!(adopted.topics, ["builds @91234"]);
        assert_eq!(
            (
                adopted.fabric.last_msg_id,
                adopted.fabric.last_post_id,
                adopted.fabric.last_receipt_id,
                adopted.fabric.uncarried.as_slice(),
                adopted.fabric.receipts_uncarried,
                adopted.fabric.dropped,
            ),
            (8192, 12288, 8192, &[4][..], 2, 9)
        );
        assert!(
            adopted
                .checkpoint
                .as_ref()
                .is_some_and(|cp| cp.modes.bracketed_paste),
            "the screen beside it still reads"
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

    /// THE META CARRIES NO IN-FLIGHT OUTPUT STATE (round eight of the update
    /// audit, finding 502's review). The pump resends the META only when the
    /// modes change, so at a crash it may be hours old: an OSC 8 link open when
    /// it was sampled, closed since, would link everything the program prints
    /// after the recovery, and a long URI alone pushed a 100-column alt-screen
    /// tab's META past `MAX_META`, so none was sent at all. `keeper_meta`
    /// sends none of that state, and `recovered_checkpoint` takes none from a
    /// META that holds it (nor a partial sequence), whichever window sent it.
    #[cfg(unix)]
    #[test]
    fn the_meta_carries_no_in_flight_output_state() {
        let mut t = Terminal::new(30, 100);
        t.process(b"\x1b[?1049h\x1b[?2004h\x1b[31m\x1b[#{\x1b[?7s");
        let uri = format!("https://example.com/{}", "a".repeat(4000));
        t.process(format!("\x1b]8;id=k;{uri}\x1b\\linked").as_bytes());
        assert_eq!(
            t.current_hyperlink().map(AsRef::as_ref),
            Some(uri.as_str()),
            "PRECONDITION: the link is open"
        );
        let full = t.carry_meta_abandoning_partial();
        assert!(
            full.output_state.hyperlink.is_some() && !full.output_state.sgr_stack.is_empty(),
            "PRECONDITION: the handoff's meta holds the in-flight state"
        );
        assert!(
            aterm_json::to_string(&full).expect("json").len() > aterm_keeper::wire::MAX_META,
            "NON-VACUITY: with it the META would not fit"
        );

        let meta = crate::seamless::keeper_meta(&t).expect("fits");
        assert!(meta.len() <= aterm_keeper::wire::MAX_META);
        let text = String::from_utf8(meta.clone()).expect("utf-8");
        assert!(!text.contains("\"output_state\""), "{text}");
        let mut used = 0;
        let cp = crate::seamless::recovered_checkpoint(&meta, &mut used).expect("a checkpoint");
        assert!(cp.modes.bracketed_paste, "the modes still cross");
        assert!(cp.output_state.is_default());
        let mut recovered = Terminal::from_checkpoint(&cp);
        assert_eq!(recovered.current_hyperlink(), None, "no link reopened");
        let at = recovered.cursor();
        recovered.process(b"later");
        assert_eq!(
            recovered.hyperlink_at(at.row, at.col),
            None,
            "later output is not linked"
        );

        // A META that does hold it (another window's, or one sent mid-sequence):
        // the recovery takes neither the state nor the partial sequence.
        t.process(b"\x1b[3");
        let mut stale = t.carry_meta_abandoning_partial();
        stale.shell_integration_nonce = None;
        assert!(
            stale.parser.is_some() && !stale.output_state.is_default(),
            "PRECONDITION"
        );
        let stale = aterm_json::to_string(&stale).expect("json").into_bytes();
        let cp = crate::seamless::recovered_checkpoint(&stale, &mut used).expect("a checkpoint");
        assert!(cp.output_state.is_default(), "no in-flight output state");
        assert!(cp.parser.is_none(), "no partial sequence");
        let mut recovered = Terminal::from_checkpoint(&cp);
        assert_eq!(recovered.current_hyperlink(), None);
        let at = recovered.cursor();
        recovered.process(b"later");
        assert_eq!(recovered.hyperlink_at(at.row, at.col), None);
        assert!(
            recovered
                .row_text(usize::from(at.row))
                .unwrap_or_default()
                .trim_end()
                .ends_with("later"),
            "no partial CSI swallowed the text"
        );
    }

    /// A full-screen program's setup, as vim, less, htop and Claude Code send
    /// it: the alternate screen, DECCKM and the keypad, SGR button mouse,
    /// bracketed paste, a kitty keyboard push and a scroll region.
    #[cfg(unix)]
    const TUI_SETUP: &[u8] =
        b"\x1b[?1049h\x1b[?1h\x1b=\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[>1u\x1b[2;40r";

    /// The META a window before round seventeen sent: the sanitized carry as
    /// the handoff's JSON, each cursor's stops a bool per column.
    #[cfg(unix)]
    fn old_form_meta(t: &Terminal) -> Vec<u8> {
        let mut meta = t.carry_meta_abandoning_partial();
        meta.shell_integration_nonce = None;
        meta.parser = None;
        meta.output_state = aterm_core::terminal::OutputStateRepr::default();
        aterm_json::to_string(&meta).expect("json").into_bytes()
    }

    /// `meta` as a window before round seventeen reads it: without the packed
    /// stops it does not know, each cursor's `tab_stops` the empty list the
    /// packed form writes. The review of R17-HANDOFF-1 (2026-10-11) fed the
    /// packed bytes to origin/main's own `recovered_checkpoint` and got what
    /// this emulation gets.
    #[cfg(unix)]
    fn as_an_older_window_reads(meta: &[u8]) -> Vec<u8> {
        let mut value: aterm_json::Value = aterm_json::from_slice(meta).expect("the META is JSON");
        for slot in ["cursor", "alt_cursor"] {
            if let Some(cursor) = value
                .as_object_mut()
                .and_then(|object| object.get_mut(slot))
                .and_then(aterm_json::Value::as_object_mut)
            {
                cursor.remove("tab_stops_packed");
            }
        }
        aterm_json::to_string(&value).expect("json").into_bytes()
    }

    /// The modes [`TUI_SETUP`] sets, read off a recovered checkpoint.
    #[cfg(unix)]
    fn assert_tui_modes(cp: &aterm_core::terminal::TerminalCheckpoint, what: &str) {
        let modes = &cp.modes;
        assert!(modes.alternate_screen, "{what}: the alt screen");
        assert!(modes.application_cursor_keys, "{what}: DECCKM");
        assert!(modes.application_keypad, "{what}: the keypad");
        assert!(modes.bracketed_paste, "{what}: bracketed paste");
        assert_eq!(
            modes.mouse_mode,
            aterm_core::terminal::MouseMode::ButtonEvent,
            "{what}: the mouse"
        );
        assert_eq!(
            modes.mouse_encoding,
            aterm_core::terminal::MouseEncoding::Sgr,
            "{what}: the mouse encoding"
        );
    }

    /// THE META FITS AT ANY WIDTH (round seventeen, R17-HANDOFF-1). Each cursor
    /// carried its tab stops as a JSON bool per column — two cursors on the
    /// alternate screen, and a vector a grid never shrinks — so from 119 alt-
    /// screen columns (330 on the main screen, or any pane once that wide) even
    /// the bare scalars passed `MAX_META`, `keeper_meta` returned nothing, and
    /// a crash recovery restored the modes from before the program started.
    /// A 200- and a 400-column alt-screen session, custom stops included, and
    /// a pane once 400 columns wide now 100: each META fits and its stops
    /// round-trip, the one past the width included.
    #[cfg(unix)]
    #[test]
    fn a_wide_alt_screen_meta_fits_and_carries_its_tab_stops() {
        let recover = |t: &Terminal, what: &str| {
            let meta = crate::seamless::keeper_meta(t)
                .unwrap_or_else(|| panic!("{what}: the META was not built"));
            assert!(
                meta.len() <= aterm_keeper::wire::MAX_META,
                "{what}: {}",
                meta.len()
            );
            let mut used = 0;
            crate::seamless::recovered_checkpoint(&meta, &mut used)
                .unwrap_or_else(|| panic!("{what}: no checkpoint"))
        };
        for cols in [200_u16, 400] {
            let what = format!("{cols} columns");
            let mut t = Terminal::new(50, cols);
            // A stop set at column 5 and the default at 16 cleared, then a stop
            // near the right edge; then the program.
            let edge = format!("\x1b[1;{}H\x1bH", cols - 2);
            t.process(b"\x1b[1;6H\x1bH\x1b[1;17H\x1b[g");
            t.process(edge.as_bytes());
            t.process(b"\x1b[H");
            t.process(TUI_SETUP);
            let carried = t.carry_meta_abandoning_partial();
            let alt = carried
                .alt_cursor
                .clone()
                .expect("PRECONDITION: an alt grid");
            assert!(
                carried.cursor.tab_stops[5] && !carried.cursor.tab_stops[16],
                "PRECONDITION: custom stops"
            );
            let cp = recover(&t, &what);
            assert_tui_modes(&cp, &what);
            assert_eq!(cp.cursor.tab_stops, carried.cursor.tab_stops, "{what}");
            let alt_cp = cp.alt_cursor.clone().expect("the saved primary's cursor");
            assert_eq!(alt_cp.tab_stops, alt.tab_stops, "{what}");
            assert_eq!(
                (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
                (1, 39),
                "{what}: the program's cursor is carried, not reset"
            );
            let recovered = Terminal::from_checkpoint(&cp);
            assert_eq!(recovered.grid().tab_stops(), t.grid().tab_stops(), "{what}");
            assert_eq!(recovered.kitty_keyboard_flags(), t.kitty_keyboard_flags());
        }

        // Once 400 columns wide, now 100: the vector keeps 400 stops, and the
        // custom one at column 300 is found again by a later widen.
        let mut t = Terminal::new(50, 400);
        t.process(b"\x1b[1;301H\x1bH\x1b[H");
        t.resize(50, 100);
        t.process(TUI_SETUP);
        assert_eq!(
            t.grid().tab_stops().len(),
            400,
            "PRECONDITION: never shrunk"
        );
        let cp = recover(&t, "once 400 columns, now 100");
        assert_tui_modes(&cp, "once wide");
        assert_eq!(cp.cursor.tab_stops.len(), 400);
        assert!(cp.cursor.tab_stops[300], "the stop past the width");
        let mut recovered = Terminal::from_checkpoint(&cp);
        recovered.resize(50, 400);
        assert!(
            recovered.grid().tab_stops()[300],
            "a widen finds the stop again"
        );
    }

    /// A META IS BUILT FOR EVERY SESSION (R17-HANDOFF-1): with the stops
    /// packed, a default session's META does not grow with the width; and the
    /// worst a session can carry — 4,096 columns with a stop at every one, on
    /// both screens, a long directory and title, a pushed colour table, marks,
    /// full kitty stacks, both DECSC slots and counters at their widest — sheds
    /// to the fixed scalars and still fits, its modes and regions intact and
    /// its stops the every-8 defaults. So does that session with every other
    /// scalar at its widest too (`widest_scalars`, after the review's finding
    /// on 2026-10-11 that the worst case left the charsets, the mouse encoding
    /// and the cursor style narrow). Each with the fullest fabric beside it
    /// (the fabric audit's fourth round): both its holds, its lease, its
    /// receipt count and its `dropped=` always cross whole, and every opt-in and queued post is
    /// named or counted — the fabric never costs a session its META. Past any
    /// session's reach, with the widest scalars, the holds' reasons may give
    /// way (`KeeperCarry::rungs`); the holds, origin by origin, never do.
    #[cfg(unix)]
    #[test]
    fn a_meta_is_built_for_every_session() {
        let size = |cols: u16| {
            let mut t = Terminal::new(50, cols);
            t.process(TUI_SETUP);
            crate::seamless::keeper_meta(&t)
                .unwrap_or_else(|| panic!("{cols} columns"))
                .len()
        };
        // From 200 columns the old form does not fit, so these are packed.
        let sizes = [200, 400, 4096].map(size);
        let spread =
            sizes.iter().max().copied().unwrap_or(0) - sizes.iter().min().copied().unwrap_or(0);
        assert!(spread <= 16, "only the numbers' digits grow: {sizes:?}");

        let cols = aterm_core::grid::MAX_GRID_COLS;
        let mut t = Terminal::new(50, cols);
        let mut setup = Vec::new();
        for col in 1..=cols {
            setup.extend_from_slice(format!("\x1b[1;{col}H\x1bH").as_bytes());
        }
        setup
            .extend_from_slice(format!("\x1b]7;file://host/{}\x1b\\", "d".repeat(2000)).as_bytes());
        setup.extend_from_slice(format!("\x1b]2;{}\x07", "t".repeat(2000)).as_bytes());
        for index in 0..256 {
            setup.extend_from_slice(format!("\x1b]4;{index};rgb:12/34/56\x1b\\").as_bytes());
        }
        setup
            .extend_from_slice(b"\x1b[#P\x1b[#P\x1b]133;A\x07$ \x1b]133;B\x07ls\x1b]133;C\x07\r\n");
        setup.extend_from_slice(
            b"\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b7",
        );
        setup.extend_from_slice(TUI_SETUP);
        setup.extend_from_slice(b"\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b[>1u\x1b[>3u\x1b[>1u\x1b7");
        for col in 1..=cols {
            setup.extend_from_slice(format!("\x1b[1;{col}H\x1bH").as_bytes());
        }
        t.process(&setup);
        let mut carry = t.carry_meta_abandoning_partial();
        let alt = carry
            .alt_cursor
            .as_ref()
            .expect("PRECONDITION: two cursors");
        assert!(
            carry.cursor.tab_stops.iter().all(|stop| *stop)
                && alt.tab_stops.iter().all(|stop| *stop),
            "PRECONDITION: a stop at every column of both grids"
        );
        assert!(
            carry.current_working_directory.is_some() && carry.title.is_some(),
            "PRECONDITION: a directory and a title"
        );
        carry.absolute_row_counter = u64::MAX;
        carry.alt_absolute_row_counter = u64::MAX;
        carry.style_fg_bits = u32::MAX;
        carry.style_bg_bits = u32::MAX;
        carry.style_flag_bits = u16::MAX;
        carry.style_protected = true;
        carry.secure_keyboard_entry = true;
        carry.taskbar_progress = Some(aterm_types::TaskbarProgress::Paused(100));
        carry.shell_integration_rev = Some("f".repeat(16));
        let widest = aterm_core::terminal::KittyKeyboardFlags::from_bits(u8::MAX);
        let kitty = &mut carry.kitty_keyboard;
        kitty.flags = widest;
        kitty.main_saved_flags = Some(widest);
        kitty.alt_saved_flags = Some(widest);
        kitty.main_stack = [widest; 8];
        kitty.alt_stack = [widest; 8];
        for saved in [&mut carry.saved_cursor_main, &mut carry.saved_cursor_alt] {
            let slot = saved.as_mut().expect("PRECONDITION: both DECSC slots");
            slot.cursor_row = aterm_core::grid::MAX_GRID_ROWS - 1;
            slot.cursor_col = cols - 1;
            slot.style_fg_bits = u32::MAX;
            slot.style_bg_bits = u32::MAX;
            slot.style_flag_bits = u16::MAX;
            slot.underline_color = Some(u32::MAX);
        }
        let widest = widest_scalars(carry.clone());
        // The fullest fabric beside it: both holds, a fleet halt over the
        // owner's stop, each with a 128-byte reason JSON doubles, the lease at
        // its ceiling, every opt-in at its longest, a full outbox, a full
        // receipt queue and `dropped=` at its ceiling.
        let ceiling = i64::MAX.unsigned_abs();
        let fabric = crate::fabric::KeeperCarry {
            hold: Some(format!("fleet {}", "\"".repeat(128))),
            more_holds: vec![format!("local {}", "\"".repeat(128))],
            ids: [ceiling; 3],
            topics: (0..crate::fabric::MAX_TOPICS)
                .map(|n| format!("{n:02}{} @{}", "t".repeat(30), u64::MAX))
                .collect(),
            queued: (0..crate::fabric::OUTBOX_CAP)
                .map(|n| ceiling - u64::try_from(n).expect("n"))
                .collect(),
            owed: u64::try_from(crate::fabric::RING_CAP).expect("cap"),
            dropped: ceiling,
            ..crate::fabric::KeeperCarry::default()
        };
        let carried = |meta: &[u8], what: &str, reasons: bool| {
            let recovered = crate::seamless::recovered_fabric(meta);
            let origins: Vec<String> = recovered
                .holds
                .iter()
                .map(|row| crate::fabric::parse_hold(row).origin)
                .collect();
            assert_eq!(origins, ["fleet", "local"], "{what}: no hold is ever shed");
            if reasons {
                let whole: Vec<String> = fabric
                    .hold
                    .iter()
                    .chain(&fabric.more_holds)
                    .cloned()
                    .collect();
                assert_eq!(recovered.holds, whole, "{what}: nor a reason");
            }
            assert_eq!(
                [
                    recovered.carry.last_msg_id,
                    recovered.carry.last_post_id,
                    recovered.carry.last_receipt_id
                ],
                fabric.ids,
                "{what}: nor the lease"
            );
            assert_eq!(recovered.carry.receipts_uncarried, fabric.owed, "{what}");
            assert_eq!(
                recovered.carry.dropped, fabric.dropped,
                "{what}: nor dropped="
            );
            let named = |n: usize| u64::try_from(n).expect("n");
            assert_eq!(
                named(recovered.topics.len()) + recovered.topics_lost,
                named(fabric.topics.len()),
                "{what}: every opt-in is named or counted"
            );
            assert_eq!(
                named(recovered.carry.uncarried.len()) + recovered.queued_lost,
                named(fabric.queued.len()),
                "{what}: every queued post is named or counted"
            );
        };
        let meta =
            crate::seamless::keeper_meta_of(carry, Some(&fabric)).expect("the worst case is built");
        assert!(meta.len() <= aterm_keeper::wire::MAX_META, "{}", meta.len());
        carried(&meta, "the worst case", true);
        let worst = meta.len();
        let mut used = 0;
        let cp = crate::seamless::recovered_checkpoint(&meta, &mut used).expect("a checkpoint");
        assert_tui_modes(&cp, "the worst case");
        assert_eq!(
            (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
            (1, 39),
            "the region stays"
        );
        let defaults = crate::seamless::default_tab_stops(usize::from(cols));
        assert_eq!(
            cp.cursor.tab_stops, defaults,
            "the custom stops were shed last"
        );
        assert_eq!(cp.alt_cursor.map(|alt| alt.tab_stops), Some(defaults));

        // And with every scalar at its widest (`widest_scalars`), past what any
        // session reaches, it still sheds to a META that fits.
        let modes = aterm_json::to_string(&widest.modes).expect("json");
        let charset = aterm_json::to_string(&widest.charset).expect("json");
        assert!(
            !modes.contains("true")
                && modes.contains("SgrPixel")
                && charset.contains("IsoLatin1Supplemental")
                && widest.history_lines == u32::MAX
                && widest.cursor.scroll_bottom == u16::MAX,
            "PRECONDITION: the widest scalars: {modes} {charset}"
        );
        let meta =
            crate::seamless::keeper_meta_of(widest, Some(&fabric)).expect("the widest is built");
        assert!(meta.len() <= aterm_keeper::wire::MAX_META, "{}", meta.len());
        // Past any session's reach, the reasons may give way; the halts never.
        carried(&meta, "the widest scalars", false);
        eprintln!(
            "the worst case: {worst} bytes; with the widest scalars: {} (cap {})",
            meta.len(),
            aterm_keeper::wire::MAX_META
        );
    }

    /// `meta` with every scalar a shed META keeps at its widest JSON
    /// (R17-HANDOFF-1's review, 2026-10-11), to bound its size: each bool
    /// `false` (a byte wider than `true`); each number, and each absent
    /// optional one, the widest its field takes back unchanged (`u64::MAX`,
    /// then `u32::MAX`, `u16::MAX`, `null` and `u8::MAX`, tried in that
    /// order); each enum its longest name; and the widest character sets in
    /// every slot. The stop vectors are left as they are.
    #[cfg(unix)]
    fn widest_scalars(
        mut meta: aterm_core::terminal::CheckpointMeta,
    ) -> aterm_core::terminal::CheckpointMeta {
        use aterm_json::Value;
        fn nth_scalar<'a>(value: &'a mut Value, n: &mut usize) -> Option<&'a mut Value> {
            match value {
                Value::Object(map) => map.values_mut().find_map(|child| nth_scalar(child, n)),
                Value::Array(items) => items.iter_mut().find_map(|child| nth_scalar(child, n)),
                scalar if *n == 0 => Some(scalar),
                _ => {
                    *n -= 1;
                    None
                }
            }
        }
        let takes_back = |value: &Value| {
            aterm_json::from_value::<aterm_core::terminal::CheckpointMeta>(value.clone())
                .ok()
                .and_then(|meta| aterm_json::to_value(&meta).ok())
                .as_ref()
                == Some(value)
        };
        let main_stops = std::mem::take(&mut meta.cursor.tab_stops);
        let alt_stops = meta
            .alt_cursor
            .as_mut()
            .map(|alt| std::mem::take(&mut alt.tab_stops));
        let mut value = aterm_json::to_value(&meta).expect("json");
        let widest_numbers = [
            Value::from(u64::MAX),
            Value::from(u32::MAX),
            Value::from(u16::MAX),
            Value::Null,
            Value::from(u8::MAX),
        ];
        let mut index = 0;
        while let Some(scalar) = nth_scalar(&mut value, &mut index.clone()) {
            let candidates: &[Value] = match scalar {
                Value::Bool(_) => &[Value::Bool(false)],
                Value::Number(_) | Value::Null => &widest_numbers,
                _ => &[],
            };
            for candidate in candidates {
                let mut trial = value.clone();
                if let Some(scalar) = nth_scalar(&mut trial, &mut index.clone()) {
                    *scalar = candidate.clone();
                }
                if takes_back(&trial) {
                    value = trial;
                    break;
                }
            }
            index += 1;
        }
        let mut meta: aterm_core::terminal::CheckpointMeta =
            aterm_json::from_value(value).expect("every trial taken back");
        meta.cursor.tab_stops = main_stops;
        if let (Some(alt), Some(stops)) = (meta.alt_cursor.as_mut(), alt_stops) {
            alt.tab_stops = stops;
        }
        let modes = &mut meta.modes;
        modes.cursor_style = aterm_types::CursorStyle::BlinkingUnderline;
        modes.mouse_mode = aterm_core::terminal::MouseMode::ButtonEvent;
        modes.mouse_encoding = aterm_core::terminal::MouseEncoding::SgrPixel;
        modes.color_scheme = aterm_types::Appearance::Light;
        modes.bidi_mode = aterm_types::BiDiMode::Disabled;
        modes.bidi_direction = aterm_types::ParagraphDirection::AutoRtl;
        meta.taskbar_progress = Some(aterm_types::TaskbarProgress::Indeterminate);
        // The longest 94-character name in G0-G3 and the longest 96-character
        // one in G1-G3, in the live state and both DECSC slots.
        let mut charset = aterm_types::CharacterSetState::default();
        for g_set in 0..4 {
            charset.designate(g_set, aterm_types::CharacterSet::NorwegianDanish);
            charset.designate_96(g_set, aterm_types::CharacterSet96::IsoLatin1Supplemental);
        }
        meta.charset = charset;
        for saved in [&mut meta.saved_cursor_main, &mut meta.saved_cursor_alt]
            .into_iter()
            .flatten()
        {
            saved.charset = charset;
        }
        meta
    }

    /// VERSION SKEW, forward: the META a window before round seventeen sent
    /// (the keeper keeps it across an update, and a crash recovers from it)
    /// still restores — its bool-per-column stops, its cursor and its modes.
    #[cfg(unix)]
    #[test]
    fn a_meta_an_older_window_sent_still_restores() {
        let mut t = Terminal::new(50, 80);
        t.process(b"\x1b[1;6H\x1bH\x1b[H");
        t.process(TUI_SETUP);
        let old = old_form_meta(&t);
        let text = String::from_utf8(old.clone()).expect("utf-8");
        assert!(
            text.contains("\"tab_stops\":[false,"),
            "PRECONDITION: the old form: {text}"
        );
        let mut used = 0;
        let cp = crate::seamless::recovered_checkpoint(&old, &mut used).expect("a checkpoint");
        assert_tui_modes(&cp, "an old META");
        let carried = t.carry_meta_abandoning_partial();
        assert_eq!(cp.cursor.tab_stops, carried.cursor.tab_stops);
        assert_eq!((cp.cursor.scroll_top, cp.cursor.scroll_bottom), (1, 39));
        assert!(cp.cursor.tab_stops[5], "the custom stop crosses");
    }

    /// VERSION SKEW, backward (a rollback, or an older build launched after a
    /// newer one crashed). A META whose whole carry the old form holds is sent
    /// in it, byte for byte, so an older window reads it exactly as before. A
    /// packed one an older window's recovery reads as the handoff's meta, which
    /// ignores the packed stops it does not know and reads each cursor's
    /// `tab_stops` as the empty list the new form writes. Its Repaint rung then
    /// resets each cursor to a blank one at the same position
    /// (`repaint_checkpoint`'s sanitize), so it keeps every mode and loses only
    /// the scroll region, margins and custom stops, which a full-screen program
    /// sets again as it repaints after the size pulse. Where a window before
    /// round seventeen sent a shed old form instead, that is a trade, not only
    /// a gain (`where_only_a_shed_old_form_fit_the_meta_goes_packed_and_whole`).
    #[cfg(unix)]
    #[test]
    fn an_older_window_reads_a_new_meta_with_its_modes() {
        let mut narrow = Terminal::new(50, 80);
        narrow.process(b"\x1b[1;6H\x1bH\x1b[H");
        narrow.process(TUI_SETUP);
        assert_eq!(
            crate::seamless::keeper_meta(&narrow),
            Some(old_form_meta(&narrow)),
            "what the old form carries goes in it"
        );

        let mut t = Terminal::new(50, 200);
        t.process(TUI_SETUP);
        let meta = crate::seamless::keeper_meta(&t).expect("fits");
        assert!(
            old_form_meta(&t).len() > aterm_keeper::wire::MAX_META
                && String::from_utf8_lossy(&meta).contains("\"tab_stops_packed\""),
            "PRECONDITION: the old form does not fit, so this one is packed"
        );
        let older = as_an_older_window_reads(&meta);
        let parsed: aterm_core::terminal::CheckpointMeta =
            aterm_json::from_slice(&older).expect("an older build parses the new form");
        assert!(parsed.modes.bracketed_paste);
        let mut used = 0;
        let cp = crate::seamless::recovered_checkpoint(&older, &mut used).expect("a checkpoint");
        assert_tui_modes(&cp, "an older reader");
        assert_eq!(cp.cursor.tab_stops.len(), 200, "the blank cursor's stops");
        assert_eq!(
            (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
            (0, 49),
            "the region is the blank cursor's"
        );
    }

    /// THE BAND (R17-HANDOFF-1's review, ruled 2026-10-11). Where the whole
    /// carry does not fit the old form but would without the directory and
    /// colours (then the title and mark state), a window before round
    /// seventeen sent it so shed, and every reader lost the directory. This one
    /// sends the packed form whole. 118 alternate-screen columns and a 60-byte
    /// directory (the review's case: 4,164 bytes whole, 4,094 shed): a newer
    /// window recovers the directory AND the program's region. An older window
    /// keeps every mode and the directory, and loses the region it used to
    /// keep: the trade `PackedTabStops`' VERSION SKEW records.
    #[cfg(unix)]
    #[test]
    fn where_only_a_shed_old_form_fit_the_meta_goes_packed_and_whole() {
        let mut t = Terminal::new(50, 118);
        t.process(format!("\x1b]7;file://host/{}\x1b\\", "d".repeat(60)).as_bytes());
        t.process(TUI_SETUP);
        let mut shed = t.carry_meta_abandoning_partial();
        let dir = shed
            .current_working_directory
            .take()
            .expect("PRECONDITION: a directory");
        shed.color = aterm_core::terminal::ColorRepr::default();
        shed.title = None;
        shed.shell = aterm_core::terminal::ShellRepr::default();
        shed.shell_integration_nonce = None;
        shed.parser = None;
        shed.output_state = aterm_core::terminal::OutputStateRepr::default();
        let (whole, shed) = (
            old_form_meta(&t).len(),
            aterm_json::to_string(&shed).expect("json").len(),
        );
        assert!(
            whole > aterm_keeper::wire::MAX_META && shed <= aterm_keeper::wire::MAX_META,
            "PRECONDITION: the band, where only a shed old form fits: {whole}, shed {shed}"
        );

        let meta = crate::seamless::keeper_meta(&t).expect("built");
        assert!(
            String::from_utf8_lossy(&meta).contains("\"tab_stops_packed\""),
            "the band goes packed, not shed"
        );
        let mut used = 0;
        let cp = crate::seamless::recovered_checkpoint(&meta, &mut used).expect("a checkpoint");
        assert_tui_modes(&cp, "a newer reader");
        assert_eq!(
            cp.current_working_directory.as_deref(),
            Some(dir.as_str()),
            "a newer reader keeps the directory"
        );
        assert_eq!(
            (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
            (1, 39),
            "and the program's region"
        );

        let cp = crate::seamless::recovered_checkpoint(&as_an_older_window_reads(&meta), &mut used)
            .expect("a checkpoint");
        assert_tui_modes(&cp, "an older reader");
        assert_eq!(
            cp.current_working_directory.as_deref(),
            Some(dir.as_str()),
            "an older reader keeps the directory it used to lose"
        );
        assert_eq!(
            (cp.cursor.scroll_top, cp.cursor.scroll_bottom),
            (0, 49),
            "and loses the region it used to keep"
        );
    }

    /// A META NOT SENT IS RETRIED (R17-HANDOFF-1): the pump recorded the
    /// fingerprint of every terminal it looked at, sent or not, so a META that
    /// could not be built — or found no link, or a full queue — was never
    /// tried again until the modes moved. It records only what it sent.
    ///
    /// AND THE FABRIC MOVES IT TOO (the fabric audit's fourth round, findings
    /// 2-5): a hold set or lifted, or a topic added, with the modes standing
    /// still sends the session's META again, its fabric as it stands now.
    /// NEGATIVE CONTROL: with neither moved, nothing is built.
    #[cfg(unix)]
    #[test]
    fn a_meta_that_was_not_sent_is_retried() {
        let term = Arc::new(Mutex::new(Terminal::new(24, 80)));
        term.lock().expect("term").process(b"\x1b[?2004h");
        let session = crate::session_store::test_handle(7);
        let state = Mutex::new(State {
            phase: Phase::Off,
            buffered: Vec::new(),
            tracked: BTreeMap::from([(
                7,
                Tracked {
                    rdev: 1,
                    term: Arc::downgrade(&term),
                    fabric: Arc::downgrade(&session.ctx.fabric),
                    last: None,
                },
            )]),
            pump: false,
        });
        let last = || state.lock().expect("state").tracked[&7].last.clone();
        let build = |meta, fabric: &crate::fabric::KeeperCarry| {
            crate::seamless::keeper_meta_of(meta, Some(fabric))
        };
        let unchanged = |_, _: &crate::fabric::KeeperCarry| -> Option<Vec<u8>> {
            panic!("an unchanged fingerprint and fabric build nothing")
        };

        assert_eq!(meta_pass_over(&state, |_, _| None), 0);
        assert_eq!(last(), None, "a META that was not built is tried again");
        assert_eq!(meta_pass_over(&state, build), 0);
        assert_eq!(last(), None, "a META no link took is tried again");

        // A link whose keeper is not there yet takes the META into its queue.
        let link = KeeperLink::start(
            std::path::PathBuf::from("/nonexistent-aterm-keeper-test/k.sock"),
            aterm_keeper::identity::Identity::same_uid(),
            "test".into(),
        )
        .expect("link");
        state.lock().expect("state").phase = Phase::Live(Arc::new(link));
        assert_eq!(meta_pass_over(&state, build), 1, "the retry sends it");
        let sent = Fingerprint::of(&term.lock().expect("term"));
        assert_eq!(
            last().map(|(fingerprint, _)| fingerprint),
            Some(sent),
            "what was sent is recorded"
        );
        assert_eq!(meta_pass_over(&state, unchanged), 0);

        // The modes stand still; the fabric moves.
        let held = crate::fabric::Hold {
            reason: "stop".to_string(),
            origin: "local".to_string(),
        };
        assert!(crate::fabric::apply_hold_for_test(&session.ctx, Some(held)));
        let sent_meta = std::sync::Mutex::new(Vec::new());
        let capture = |meta, fabric: &crate::fabric::KeeperCarry| {
            let bytes = crate::seamless::keeper_meta_of(meta, Some(fabric));
            sent_meta
                .lock()
                .expect("capture")
                .clone_from(bytes.as_ref().expect("a META"));
            bytes
        };
        assert_eq!(meta_pass_over(&state, capture), 1, "a hold sends it");
        let recovered = crate::seamless::recovered_fabric(&sent_meta.lock().expect("capture"));
        assert_eq!(recovered.holds, ["local stop"]);
        assert_eq!(meta_pass_over(&state, unchanged), 0);
        // A fleet halt over it is a second slot, and sends it too.
        let halt = crate::fabric::Hold {
            reason: "halt".to_string(),
            origin: "fleet".to_string(),
        };
        assert!(crate::fabric::apply_hold_for_test(&session.ctx, Some(halt)));
        assert_eq!(meta_pass_over(&state, capture), 1, "a second hold sends it");
        let recovered = crate::seamless::recovered_fabric(&sent_meta.lock().expect("capture"));
        assert_eq!(recovered.holds, ["fleet halt", "local stop"]);
        assert_eq!(meta_pass_over(&state, unchanged), 0);
        assert_eq!(
            session
                .ctx
                .fabric
                .topic_add("builds", "head", &session.ctx.timeline),
            Ok(true)
        );
        assert_eq!(meta_pass_over(&state, capture), 1, "a topic sends it");
        assert!(crate::fabric::apply_hold_for_test(&session.ctx, None));
        assert_eq!(meta_pass_over(&state, capture), 1, "a lift sends it");
        let recovered = crate::seamless::recovered_fabric(&sent_meta.lock().expect("capture"));
        assert_eq!(
            (recovered.holds, recovered.topics),
            (Vec::<String>::new(), vec!["builds head".to_string()]),
            "the last META sent is the fabric as it stands"
        );
        assert_eq!(meta_pass_over(&state, unchanged), 0);
        let phase = std::mem::replace(&mut state.lock().expect("state").phase, Phase::Off);
        if let Phase::Live(link) = phase
            && let Ok(link) = Arc::try_unwrap(link)
        {
            link.shutdown();
        }
    }
}
