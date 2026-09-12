// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **A9 — THE FILE MIRROR**, for an agent whose sandbox reaches no socket.
//!
//! §9.1 names the case exactly: Codex "has no hook path here … and no socket
//! path (T13, RED): its path is the file mirror, A9". A process confined to a
//! writable root cannot `connect(2)` to aterm's control socket, so its whole
//! participation in the fabric has to be files inside that root:
//!
//! ```text
//! <root>/.aterm/<sid>/inbox.ndjson    read  — one JSON object per delivered row
//! <root>/.aterm/<sid>/outbox.ndjson  write — one JSON object per message to send
//! <root>/.aterm/<sid>/sent.ndjson    read  — what aterm answered for each of those
//! <root>/.aterm/<sid>/.cursor              — how much of outbox.ndjson was consumed
//! ```
//!
//! A line of `inbox.ndjson` is a delivered row, EXCEPT for a `"notice"` line —
//! today only the eviction count the endpoint reports (see [`dropped_notice`]).
//! A notice carries no `off`, so a reader keyed on the row vocabulary skips it.
//!
//! `inbox.ndjson` is **`off=`-idempotent**: the broker offset is the row's
//! identity, exactly as it is at `deliver` (§11.2), and a row already in the
//! file is never appended a second time — not by a re-listing, not by a mirror
//! restart, not by aterm redelivering after a refill. Nothing else in this
//! module is allowed to be the identity: message ids are per-session counters
//! that a relaunch resets, and text is not unique.
//!
//! ## Relaying never launders authority
//!
//! An outbox line may carry `"via"`. That makes the message a RELAY on the
//! writer's own word, and §6.7's rule is unconditional: "A relayed message is
//! always delivered as `kind=note demoted=<k> trust=relayed`, whatever the
//! recipient's allowlist says". This module implements that rule by **not
//! implementing it**. The `via=` claim is carried onto the wire by the ordinary
//! `post` verb and the demotion is computed where it already is — by the
//! RECEIVING bridge, in [`crate::bridge::Bridge::classify_kind`], which demotes
//! on `via.is_some()` before it ever consults `--accept-from`. A second copy of
//! the rule here would be a second chance to disagree with it, and the audit's
//! standing lesson is that two subsystems reading one value is exactly where
//! this project's worst defect lived.
//!
//! ## The root is hostile ground
//!
//! Everything under `<root>` was authored by the process being mirrored, which
//! is the process the sandbox exists to contain. Three consequences are built
//! in rather than assumed:
//!
//! * **No path comes from a file's contents, and the directory it names is
//!   checked at every use.** The only paths opened are `<root>/.aterm/<sid>/...`
//!   for a `<sid>` aterm itself listed; every open refuses a leaf that is not a
//!   private regular file of this process's own — not a symlink, not a hard link
//!   to somewhere else, not a fifo or a device — and every open, rename and stat
//!   first re-checks that `<root>/.aterm/<sid>` is still the directory this
//!   process validated, against a descriptor held open since it did ([`Plane`]).
//!   Both halves are load-bearing: a leaf that names a file outside the root
//!   turns a peer's message text into an append to that file under the mirror's
//!   authority rather than the agent's, and so - until the plane was pinned -
//!   did swapping a DIRECTORY component at any time after the one-time check.
//!   THE LEAF GUARD IS NOT A SYMLINK GUARD, and it was: `ln <victim>
//!   <root>/.aterm/<sid>/inbox.ndjson` reaches the identical outcome with no
//!   race and no symlink, `lstat` reports it as an ordinary file and
//!   `O_NOFOLLOW` is satisfied by it. [`Plane::leaf_is_private`] states the rule
//!   the four files actually keep. What the pin does not close, and what would,
//!   is stated on [`Plane`] rather than assumed.
//! * **No outbox field is interpolated into a control line unvalidated.** `to`,
//!   `kind`, `re`, `dl` and `via` are checked against the same grammars the
//!   endpoint checks, and the message body travels as `post … len=<n>` +
//!   raw bytes — a length-prefixed frame, so a body holding a newline (or the
//!   text `deliver s-x off=1 …`) cannot become a second verb on the connection.
//!   That is the file-plane instance of the rule the whole design rests on: a
//!   record's body never becomes input by any path.
//! * **Nothing durable and unbounded.** `inbox.ndjson` rotates at
//!   [`ROTATE_BYTES`] to `inbox.ndjson.1` (two generations, then the oldest is
//!   dropped), the idempotency window is [`DEDUP_KEEP`] offsets seeded from the
//!   file's own tail, and a single outbox line over [`LINE_MAX`] is refused
//!   rather than buffered.
//!
//! ## What authority this needs — none of the bridge's
//!
//! The mirror speaks only `sessions`, `inbox` (`--peek`, `get` and `seen`) and
//! `post`: ordinary `Scoped` verbs that every in-session client already holds
//! (§11.2). `inbox seen` is the one it issues on the agent's behalf rather than
//! at the agent's word, and [`run_inbox`] says exactly what that claims. **It
//! never calls a `BridgeOnly` verb**, so the sandbox-facing plane cannot reach
//! the bridge plane even by a bug in this file. That sentence is not left to a
//! reader's grep: [`tests::the_mirror_names_no_bridge_only_verb`] reads this
//! module's own source and fails if one appears. The USAGE trailer below says
//! why it is a subcommand of its own rather than a `serve` flag.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::ctl::{Ctl, Reply};

/// How long between polls of aterm and of `outbox.ndjson`.
///
/// A POLL, said out loud: there is no inotify here and no dependency that would
/// bring one, and the design's own budget for this path is a file an agent
/// writes between turns. The cost is one `inbox --peek` and one `stat` per
/// mirrored session per interval, and the latency it buys is a quarter second.
const POLL_DEFAULT: Duration = Duration::from_millis(250);
const POLL_MIN: Duration = Duration::from_millis(10);
const POLL_MAX: Duration = Duration::from_secs(60);

/// The largest single outbox line accepted, matching the endpoint's `BODY_MAX`.
/// A longer one is refused and skipped rather than buffered: an unbounded read
/// off a file a sandboxed process writes is a memory bomb with a file handle.
const LINE_MAX: usize = 256 * 1024;

/// The most outbox bytes read in one poll. Bounds the work per round when an
/// agent appends a burst.
const READ_MAX: u64 = 1024 * 1024;

/// How many delivered offsets the idempotency window remembers, and how much of
/// `inbox.ndjson`'s tail is scanned to seed it.
///
/// EXACT INSIDE THE WINDOW, CONSERVATIVE BELOW IT: an offset in the set is a
/// duplicate, and an offset below the oldest one in the set is treated as
/// already written. The alternative — an unbounded set — is the durable
/// unbounded thing this rung must not add, and the alternative to THAT — a bare
/// high-water mark — would silently drop a row that the endpoint refused once
/// (`ERR quota`) and accepted later at a lower offset.
const DEDUP_KEEP: usize = 4096;
const TAIL_SCAN: u64 = 1024 * 1024;

/// The size at which `inbox.ndjson` is rotated to `inbox.ndjson.1`.
const ROTATE_BYTES: u64 = 8 * 1024 * 1024;

/// The kinds `post` accepts (§11.2). `expired` and `undeliverable` are verdicts
/// a bridge records, never something a sender may claim.
const POSTABLE: [&str; 7] = ["ask", "answer", "task", "report", "note", "ack", "control"];

/// `O_NOFOLLOW` for the platforms whose value is known here; `0` elsewhere,
/// where [`Plane::open_guarded`]'s `symlink_metadata` check stands alone.
#[cfg(any(target_os = "linux", target_os = "android"))]
const O_NOFOLLOW: i32 = 0x0002_0000;
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
))]
const O_NOFOLLOW: i32 = 0x0000_0100;
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "watchos",
    target_os = "freebsd",
    target_os = "netbsd",
    target_os = "openbsd",
    target_os = "dragonfly"
)))]
const O_NOFOLLOW: i32 = 0;

// ---------------------------------------------------------------------------
// options
// ---------------------------------------------------------------------------

/// Everything `aterm-link mirror` was told.
#[derive(Debug, Clone)]
pub struct Options {
    /// The sandbox root. Every file this process reads or writes is under
    /// `<root>/.aterm/`, and nothing outside it holds any of the mirror's state.
    pub root: PathBuf,
    /// aterm's control socket.
    pub sock: String,
    /// The instance token file. Defaults to the one `aterm_uds::latest` names
    /// for `sock`.
    pub token_file: Option<String>,
    /// Which sessions to mirror. EMPTY MEANS EVERY SESSION the instance hosts —
    /// convenient for the one-session case the design describes, and a
    /// disclosure if the root belongs to one agent and the instance hosts
    /// others, so name them when it does.
    ///
    /// AND NOT ONLY A DISCLOSURE: the mirror acknowledges what it has written
    /// (`inbox seen`, see [`run_inbox`]), so mirroring a session whose agent has
    /// a socket of its own moves THAT agent's watermarks — mail it never read
    /// would read as delivered. Mirror the sandboxed sessions, by name, whenever
    /// the instance hosts anything else.
    pub sessions: Vec<String>,
    pub interval: Duration,
}

/// Usage, printed on a usage error.
const USAGE: &str = "\
aterm-link mirror — the file plane for an agent that reaches no socket (A9)

  aterm-link mirror <root> --sock <path> [options]

  <root>, or --mirror <root>   the sandbox root; the plane is <root>/.aterm/<sid>/
  --sock <path>                aterm's control socket
  --token-file <path>          the instance token (default: the socket's own)
  --session <sid>              mirror only this session; repeatable.
                               DEFAULT: EVERY session the instance hosts. The
                               mirror runs `inbox seen` on the agent's behalf for
                               what it has written, so mirroring a session that
                               has a socket client of its own moves THAT agent's
                               watermarks — mail it never read reads as
                               delivered. Name the sandboxed sessions whenever
                               the instance hosts anything else.
  --interval <ms>              poll interval (default 250, min 10, max 60000)

  <root>/.aterm/<sid>/inbox.ndjson   one JSON object per delivered row, off=-idempotent,
                                     plus a \"notice\" line when aterm reports a drop
  <root>/.aterm/<sid>/outbox.ndjson  append {\"to\":…,\"kind\":…,\"text\":…} to send
  <root>/.aterm/<sid>/sent.ndjson    what aterm answered for each line

`--mirror <root>` is §11.2's spelling of the root and is accepted as such. This
is a subcommand rather than a flag on `serve`: the mirror needs no bridge
authority at all — `sessions`, `inbox` (`--peek`/`get`/`seen`) and `post` are
ordinary Scoped verbs — so keeping it out of the bridge process keeps the plane a
sandboxed agent writes from ever reaching the bridge plane.
";

/// Parse `aterm-link mirror`'s argv.
///
/// # Errors
///
/// A missing value, an unknown flag, a root or socket that was not given.
pub fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut root: Option<PathBuf> = None;
    let mut sock: Option<String> = None;
    let mut token_file: Option<String> = None;
    let mut sessions: Vec<String> = Vec::new();
    let mut interval = POLL_DEFAULT;
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            // §11.2 spells the root `--mirror <root>`; `--root` reads better on
            // a subcommand already called `mirror`. Both, one meaning.
            "--mirror" | "--root" => root = Some(PathBuf::from(value()?)),
            "--sock" => sock = Some(value()?),
            "--token-file" => token_file = Some(value()?),
            "--session" => {
                let sid = value()?;
                if !crate::subject::is_principal(&sid) || !sid.starts_with("s-") {
                    return Err(format!("--session {sid} is not a session id"));
                }
                sessions.push(sid);
            }
            "--interval" => {
                let ms: u64 = value()?
                    .parse()
                    .map_err(|_| "--interval takes milliseconds".to_string())?;
                interval = Duration::from_millis(ms).clamp(POLL_MIN, POLL_MAX);
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            positional if root.is_none() => root = Some(PathBuf::from(positional)),
            other => return Err(format!("unexpected argument {other}")),
        }
    }
    let (Some(root), Some(sock)) = (root, sock) else {
        return Err("a root and --sock are required".to_string());
    };
    Ok(Options {
        root,
        sock,
        token_file,
        sessions,
        interval,
    })
}

/// `aterm-link mirror` — run both directions until aterm goes away.
#[must_use]
pub fn main(args: &[String]) -> ExitCode {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("aterm-link mirror: {e}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let dir = opts.root.join(".aterm");
    if let Err(e) = ensure_dir(&dir) {
        eprintln!("aterm-link mirror: {}: {e}", dir.display());
        return ExitCode::FAILURE;
    }
    // TWO CONNECTIONS, ONE PER DIRECTION, and that is not tidiness. `post` waits
    // for its landing by default for `ask`/`task` (§11.2), so the outbound side
    // can park for tens of seconds on one line; sharing a connection with the
    // inbound side would stop mirroring incoming rows for exactly as long — and
    // an agent that cannot see the answer arrive is the failure this whole rung
    // exists to prevent.
    // WHEN ONE PLANE STOPS, THE PROCESS STOPS. Either thread ends only on a
    // LOST control lane (see [`run_outbox`]), which is aterm going away — and
    // the other lane is to the same aterm, so carrying on with half a mirror
    // would leave a sandboxed agent reading mail it can no longer answer, with
    // one stderr line as the only notice. `stop` is checked at the top of each
    // poll, so the survivor exits within one `--interval`, and the exit is
    // FAILURE so a supervisor relaunches rather than reading it as a clean end.
    let stop = Arc::new(AtomicBool::new(false));
    let inbound = opts.clone();
    let inbound_stop = Arc::clone(&stop);
    let reader = std::thread::spawn(move || run_inbox(&inbound, &inbound_stop));
    let out = run_outbox(&opts, &stop);
    stop.store(true, Ordering::Relaxed);
    if let Err(e) = out {
        eprintln!("aterm-link mirror: the outbound side stopped: {e}");
    }
    if let Ok(Err(e)) = reader.join() {
        eprintln!("aterm-link mirror: the inbound side stopped: {e}");
    }
    ExitCode::FAILURE
}

/// Send one request on a mirror connection, REFUSING any `BridgeOnly` verb.
///
/// **THE CONTAINMENT CLAIM, ENFORCED RATHER THAN WRITTEN.** The module header
/// says this plane never calls a `BridgeOnly` verb, and that sentence is the
/// whole of §8.5's confused-deputy argument for the one plane a contained,
/// hostile-by-assumption agent writes into. A sentence is not a mechanism: every
/// request this module sends goes through here, and the set it is checked
/// against is READ FROM THE VERB TABLE (`aterm_types::control_verbs`), never
/// copied — a verb reclassified `BridgeOnly` tomorrow is refused here today.
///
/// The refusal is local: nothing is written, so the lane is untouched and the
/// caller records it the way it records any other refusal.
///
/// # Errors
///
/// `PermissionDenied` for a `BridgeOnly` verb; otherwise as [`Ctl::request`].
fn scoped(ctl: &mut Ctl, line: &str, body: &[u8]) -> io::Result<Reply> {
    if let Some(verb) = bridge_only_verb(line) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("`{verb}` is a BridgeOnly verb; the file plane never sends one"),
        ));
    }
    ctl.request_with_body(line, body)
}

/// Whether the control lane SURVIVED an error from [`scoped`] — a local refusal
/// that wrote nothing — rather than being lost.
///
/// **THE DISTINCTION ctl.rs SPELLS OUT AND THIS MODULE USED TO IGNORE.**
/// [`Ctl`] latches a lane on the first I/O failure and refuses two things
/// WITHOUT WRITING A BYTE: a request line over [`crate::ctl::REQUEST_LINE_MAX`]
/// (`InvalidInput`) and a `BridgeOnly` verb (`PermissionDenied`, added here). In
/// both of those the stream is still framed and aterm is still there, so ending
/// the plane over one is throwing away every later message on this root because
/// of one line the agent wrote — the exact loss [`Taken::TooLong`] exists to
/// prevent for the sibling case. Only a LOST lane means "aterm has gone away",
/// and only that ends a plane.
///
/// It is `!lost()` and not a list of error kinds ON PURPOSE: an error kind added
/// to [`Ctl`] tomorrow is classified correctly here today, and the property that
/// matters — "nothing was written, so this line can be retired with a verdict" —
/// is exactly what the latch tracks.
fn lane_survived(ctl: &Ctl) -> bool {
    !ctl.lost()
}

/// The `BridgeOnly` verb a request line names, if it names one.
///
/// The selector is stripped exactly as `ctl::request_verb` strips it, and BOTH
/// the first word and the first two are looked up, because the table has
/// two-word verbs (`outbox sent`) and a future one could sit under a head word
/// that is `Scoped` on its own.
fn bridge_only_verb(line: &str) -> Option<String> {
    let no_sel = line
        .strip_prefix('@')
        .and_then(|rest| rest.split_once(' ').map(|(_, tail)| tail))
        .unwrap_or(line);
    let mut words = no_sel.split_whitespace();
    let one = words.next()?;
    let two = words.next().map(|w| format!("{one} {w}"));
    [Some(one.to_string()), two]
        .into_iter()
        .flatten()
        .find(|verb| aterm_types::control_verbs::is_bridge_only(verb))
}

/// One authenticated control connection.
fn connect(opts: &Options) -> io::Result<Ctl> {
    let path = match &opts.token_file {
        Some(p) => PathBuf::from(p),
        None => aterm_uds::latest::token_path_for_sock(&opts.sock).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "no token file for that socket; pass --token-file",
            )
        })?,
    };
    let token = std::fs::read_to_string(&path)?.trim().to_string();
    Ctl::connect(&opts.sock, &token)
}

/// The sessions to mirror: what aterm lists, filtered by `--session`.
fn sessions(ctl: &mut Ctl, opts: &Options) -> io::Result<Vec<String>> {
    let reply = scoped(ctl, "sessions", &[])?;
    if !reply.ok() {
        return Err(io::Error::other(format!("sessions: {}", reply.header())));
    }
    Ok(reply
        .rows()
        .iter()
        .filter_map(|row| row.split_whitespace().nth(1).map(str::to_string))
        .filter(|sid| crate::subject::is_principal(sid) && sid.starts_with("s-"))
        .filter(|sid| opts.sessions.is_empty() || opts.sessions.contains(sid))
        .collect())
}

// ---------------------------------------------------------------------------
// the inbound direction
// ---------------------------------------------------------------------------

/// Poll `inbox --peek` and append every row the file does not already hold, then
/// acknowledge what landed.
///
/// **THE LISTING PEEKS; THE ACKNOWLEDGEMENT IS SEPARATE AND EXACT.** A bare
/// `inbox` marks every row it returns listed whether or not that row reached the
/// file, and the file is the only thing that reaches this agent. So the listing
/// peeks, and what is acknowledged is `inbox seen <id>` for the highest row
/// DURABLY IN `inbox.ndjson` — the rows below it included, since the watermark
/// is monotone and `since` never steps over a row whose append failed.
///
/// WITHOUT THAT ACK THE PLANE DEADLOCKS, which is why it is here rather than
/// left as the agent's own act. §9.1's mirrored agent reaches no socket — that
/// is the whole reason A9 exists — so it can never run `inbox seen` itself, and
/// nothing in the file protocol carries an acknowledgement inbound. The
/// endpoint's per-sender quota counts UNLISTED rows (`fabric.rs`: 64, then `ERR
/// quota`, with no eviction below the cap), so a mirrored session that had read
/// and acted on every message it was ever sent was nonetheless permanently
/// unreachable by the peer that sent them.
///
/// WHAT THE ACK CLAIMS, EXACTLY: "row `<id>` and everything before it is in the
/// agent's inbox file." It is NOT a claim that the agent acted on it. On this
/// plane those are as close as they get — the file IS the delivery — but the
/// endpoint's `seen=`, the timeline's `inbox-seen`, and the `seen_off` the
/// bridge persists from it therefore mean "mirrored" for a mirrored session.
/// That is the right meaning for the one thing that reads `seen_off`: the ring
/// refill after an instance relaunch starts above it, and starting above a row
/// that is durably in the file is correct — the file's `off=` idempotency would
/// drop the re-delivery anyway.
///
/// AND THE COST, SAID OUT LOUD: this moves watermarks that belong to the session,
/// so a session with a socket client of its own must not be mirrored. See
/// [`Options::sessions`], which is how you say which ones are.
fn run_inbox(opts: &Options, stop: &AtomicBool) -> io::Result<()> {
    let mut ctl = connect(opts)?;
    let dir = opts.root.join(".aterm");
    let mut files: BTreeMap<String, InboxFile> = BTreeMap::new();
    let mut warned = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // ONE OPEN DESCRIPTOR PER MIRRORED SESSION, AND NOT ONE MORE. Each entry
        // holds its session directory open ([`Plane`]), so an entry that outlives
        // its session is a descriptor that never comes back — on a long-lived
        // mirror over an instance that opens and closes sessions, that is EMFILE
        // rather than a leak of a few bytes. Dropping the entry is safe by the
        // module's own argument: everything it held is derived from the files,
        // and re-opening re-seeds the `off=` window from the file's own tail,
        // which is exactly what a mirror restart does.
        let live = match sessions(&mut ctl, opts) {
            Ok(live) => {
                warned = false;
                live
            }
            // A REPLY THE LANE SURVIVED IS NOT THE END OF THIS PLANE. An `ERR`
            // to `sessions` used to unwind the thread, so one refusal took the
            // mirror down for the life of the process — with the process still
            // running. Reported once per run of failures, retried at the poll
            // interval; see [`lane_survived`].
            Err(e) if lane_survived(&ctl) => {
                if !warned {
                    eprintln!("aterm-link mirror: sessions: {e}");
                    warned = true;
                }
                std::thread::sleep(opts.interval);
                continue;
            }
            Err(e) => return Err(e),
        };
        files.retain(|sid, _| live.contains(sid));
        for sid in live {
            let file = match files.entry(sid.clone()) {
                std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::btree_map::Entry::Vacant(e) => {
                    match InboxFile::open(&dir, &sid) {
                        Ok(f) => e.insert(f),
                        Err(err) => {
                            eprintln!("aterm-link mirror: {sid}: {err}");
                            continue;
                        }
                    }
                }
            };
            // `since=` keeps the reply small in steady state. It is deliberately
            // NOT seeded from the file on startup: a fresh process re-lists
            // everything the ring still holds, which is what makes the file's
            // `off=` idempotency load-bearing rather than decorative.
            let listing = match scoped(
                &mut ctl,
                &format!("@{sid} inbox --peek since={}", file.since),
                &[],
            ) {
                Ok(listing) => listing,
                Err(e) if lane_survived(&ctl) => {
                    file.report(&format!("{sid} inbox --peek"), &e);
                    continue;
                }
                Err(e) => return Err(e),
            };
            if !listing.ok() {
                continue;
            }
            // AN EVICTION IS REPORTED ON THIS PLANE TOO. `dropped=` is the
            // endpoint's count of rows evicted before anyone listed them, and
            // `fabric.rs` promises an eviction is "REPORTED, never silent" — but
            // the mirror read only the rows and threw the header away, so the
            // one agent with no other channel was the one that never heard.
            // Reported as a total, so a mirror restart re-states the count
            // rather than hiding drops that happened while it was down.
            if let Some((dropped, notice)) = dropped_notice(listing.header(), file.dropped) {
                match file.notice(&notice) {
                    Ok(()) => {
                        file.dropped = dropped;
                        file.last_error = None;
                    }
                    Err(e) => file.report(&format!("{sid} inbox.ndjson"), &e),
                }
            }
            for line in listing.rows() {
                let Some(row) = parse_msg_row(line) else {
                    continue;
                };
                if file.holds(row.off) {
                    file.since = file.since.max(row.id);
                    continue;
                }
                // The listing truncates at 512 B with `more=1`, so a row that
                // says so is re-read whole with `inbox get`. THE FILE CARRIES
                // WHATEVER THE ENDPOINT CAN HAND OVER, WHICH IS NOT ALWAYS THE
                // WHOLE BODY: the delivering bridge cuts a body that would not
                // fit on a 64 KiB control line while `len=` still names the true
                // size, and `inbox get` answers what was stored. So the agent
                // never has to GUESS what was cut — `row_json` writes
                // `"truncated": true` whenever fewer bytes were written than
                // `len` declares — but it is not promised the whole of every
                // body, and this comment used to say it was.
                let text = if row.more {
                    match scoped(&mut ctl, &format!("@{sid} inbox get {}", row.id), &[]) {
                        Ok(r) if r.ok() => String::from_utf8_lossy(r.body()).into_owned(),
                        // The preview, and `row_json`'s `"truncated": true` says
                        // so — never a body silently short of its own `len`.
                        _ => row.text.clone(),
                    }
                } else {
                    row.text.clone()
                };
                if let Err(e) = file.append(&row, &text) {
                    // STOP AT THE FIRST FAILURE, and do not move `since` past
                    // it. `since=` is the endpoint's filter: stepping over a row
                    // that never reached the file would mean the row is never
                    // listed again and the agent's only channel lost it in
                    // silence — and the rows after it would be acknowledged as
                    // delivered while one in the middle was not.
                    file.report(&format!("{sid} inbox.ndjson"), &e);
                    break;
                }
                file.last_error = None;
                file.since = file.since.max(row.id);
            }
            if file.since > file.acked {
                // ONE ACK PER POLL AT MOST, and it retires the attempt whatever
                // the answer: a row the ring has already evicted answers `ERR no
                // such message`, and re-asking every 250 ms forever would be a
                // loop with no end and no effect.
                let acked = file.since;
                match scoped(&mut ctl, &format!("@{sid} inbox seen {acked}"), &[]) {
                    Ok(_) => file.acked = acked,
                    Err(e) if lane_survived(&ctl) => {
                        file.report(&format!("{sid} inbox seen"), &e);
                    }
                    Err(e) => return Err(e),
                }
            }
        }
        std::thread::sleep(opts.interval);
    }
}

/// The notice for an eviction the endpoint has reported since `last`, as
/// `(the new total, the line)`, or `None` when nothing has been evicted.
///
/// `dropped=` is the endpoint's count of rows evicted before anyone listed them,
/// and `fabric.rs` promises an eviction is "REPORTED, never silent" — but the
/// mirror read only the rows and threw the header away, so the one agent with no
/// other channel was the one that never heard. It is reported as a TOTAL as well
/// as a delta, so a mirror restart re-states the count rather than hiding drops
/// that happened while it was down; the alternative, seeding the total from the
/// file, would make the agent's own file the authority on what the endpoint
/// dropped.
fn dropped_notice(header: &str, last: u64) -> Option<(u64, String)> {
    let dropped = header_number(header, "dropped");
    if dropped <= last {
        return None;
    }
    Some((
        dropped,
        format!(
            "{{\"notice\":\"dropped\",\"dropped\":{dropped},\"new\":{}}}",
            dropped - last
        ),
    ))
}

/// The value of `key=<n>` in a reply header, or `0`.
///
/// Total over anything the header could hold: a field that is missing or is not
/// a number reads as zero rather than stopping the poll.
fn header_number(header: &str, key: &str) -> u64 {
    header
        .split_whitespace()
        .find_map(|tok| tok.strip_prefix(key)?.strip_prefix('='))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// One session's `inbox.ndjson`, and the idempotency window over it.
#[derive(Debug)]
pub struct InboxFile {
    /// The validated, pinned session directory. Every open below goes through
    /// it; see [`Plane`] for what it refuses and what it cannot.
    plane: Plane,
    path: PathBuf,
    /// What `inbox.ndjson` was when this process last opened or rotated it.
    id: FileId,
    /// The `since=` the next listing asks for — the highest message id DURABLY
    /// IN THE FILE. It does not advance past a row whose append failed, because
    /// the endpoint would then never list that row again and the agent's only
    /// channel would have lost it silently.
    since: u64,
    /// The highest id acknowledged to the endpoint with `inbox seen`. See
    /// [`run_inbox`] for what that acknowledgement claims.
    acked: u64,
    /// The endpoint's `dropped=` as of the last notice written into the file.
    dropped: u64,
    /// The offsets known to be in the file, newest [`DEDUP_KEEP`].
    written: BTreeSet<u64>,
    /// The last failure reported for this session, so an identical one is not
    /// reported again. See [`InboxFile::report`].
    last_error: Option<String>,
}

impl InboxFile {
    /// Open (creating) one session's mirror directory and seed the window from
    /// the file's own tail.
    ///
    /// # Errors
    ///
    /// A directory or file that cannot be created, or one that is a symlink.
    pub fn open(aterm_dir: &Path, sid: &str) -> io::Result<Self> {
        let plane = Plane::open(aterm_dir, sid)?;
        let path = plane.path("inbox.ndjson");
        let id = plane.touch(&path)?;
        let mut written = seed_offsets(&plane, &path);
        if written.len() < DEDUP_KEEP {
            // One rotation back, so a rotation does not re-append the rows that
            // crossed it.
            for off in seed_offsets(&plane, &plane.path("inbox.ndjson.1")) {
                written.insert(off);
            }
            trim(&mut written);
        }
        Ok(Self {
            plane,
            path,
            id,
            since: 0,
            acked: 0,
            dropped: 0,
            written,
            last_error: None,
        })
    }

    /// Report one failure, ONCE per distinct message.
    ///
    /// The mirror polls four times a second forever and does not step `since`
    /// over a row it failed to write, so a plane that has been swapped — or a
    /// file the agent deleted — fails identically on every poll. Without the
    /// latch that is one refusal turned into an unbounded stream of identical
    /// lines on the operator's stderr, which is this project's own definition of
    /// a fix that opened a new hole. The message is cleared on the next success,
    /// so a NEW failure is always said out loud.
    fn report(&mut self, what: &str, e: &io::Error) {
        let line = format!("{what}: {e}");
        if self.last_error.as_deref() == Some(line.as_str()) {
            return;
        }
        eprintln!("aterm-link mirror: {line}");
        self.last_error = Some(line);
    }

    /// Whether the file already holds `off` — exact inside the window,
    /// conservative below it (see [`DEDUP_KEEP`]).
    #[must_use]
    pub fn holds(&self, off: u64) -> bool {
        if self.written.contains(&off) {
            return true;
        }
        self.written.len() >= DEDUP_KEEP && self.written.first().is_some_and(|oldest| off < *oldest)
    }

    /// Append one row, rotating first if the file has grown past
    /// [`ROTATE_BYTES`].
    ///
    /// # Errors
    ///
    /// Any I/O failure, including a symlink where the file should be.
    pub fn append(&mut self, row: &Row, text: &str) -> io::Result<()> {
        // THE IDEMPOTENCY IS HERE, not only at the call site. One place decides
        // whether an offset is already in the file, so a second caller — a
        // refill, a re-listing, a later rung — cannot get a different answer.
        if self.holds(row.off) {
            return Ok(());
        }
        let mut line = row_json(row, text);
        line.push('\n');
        self.write_line(&line)?;
        self.written.insert(row.off);
        trim(&mut self.written);
        Ok(())
    }

    /// Write one notice line — not a delivered row, and not part of the `off=`
    /// window. See [`run_inbox`] for the one that exists: an eviction the
    /// endpoint reported in `dropped=`, which otherwise reaches every plane
    /// except the one whose agent has no other channel.
    ///
    /// # Errors
    ///
    /// Any I/O failure, including a plane that no longer verifies.
    pub fn notice(&mut self, line: &str) -> io::Result<()> {
        let mut out = line.to_string();
        out.push('\n');
        self.write_line(&out)
    }

    /// Rotate if the file has grown past [`ROTATE_BYTES`], then append one whole
    /// line.
    fn write_line(&mut self, line: &str) -> io::Result<()> {
        // THE PLANE IS CHECKED BEFORE THE SIZE IS READ, not only before the
        // write: `metadata` and `rename` resolve the same swappable path the
        // open does.
        self.plane.verify()?;
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= ROTATE_BYTES) {
            let to = self.path.with_extension("ndjson.1");
            let _ = self.plane.rotate(&self.path, &to);
            // A rotation is a NEW file, so the pin moves with it. Without this
            // the next append would take the replacement path and re-check the
            // directory for nothing.
            self.id = self.plane.touch(&self.path)?;
        }
        // ONE `write_all` for the whole line: a reader tailing the file must
        // never see half a row, and a row split across two writes is exactly how
        // it would.
        self.plane
            .open_tracked(&self.path, &mut self.id, OpenOptions::new().append(true))?
            .write_all(line.as_bytes())
    }
}

/// Keep the newest [`DEDUP_KEEP`] offsets.
fn trim(set: &mut BTreeSet<u64>) {
    while set.len() > DEDUP_KEEP {
        let Some(oldest) = set.iter().next().copied() else {
            break;
        };
        set.remove(&oldest);
    }
}

/// The `off` of every row in the tail of `path`.
fn seed_offsets(plane: &Plane, path: &Path) -> BTreeSet<u64> {
    let mut out = BTreeSet::new();
    for line in plane.tail_lines(path, TAIL_SCAN) {
        if let Some(off) = json_object(&line)
            .and_then(|m| m.get("off").cloned())
            .and_then(|v| v.parse::<u64>().ok())
        {
            out.insert(off);
        }
    }
    trim(&mut out);
    out
}

// ---------------------------------------------------------------------------
// the outbound direction
// ---------------------------------------------------------------------------

/// Poll `outbox.ndjson` and turn each complete new line into one `post`.
///
/// **EVERY LINE GETS A VERDICT, AND ONLY A LOST LANE ENDS THE PLANE.** The
/// cursor moves past a line exactly when `sent.ndjson` has said what became of
/// it — an `OK`, an `ERR`, or a local refusal named as an error — so a confined
/// agent, whose only evidence is those two files, can always tell "sent" from
/// "refused" from "not yet". A control-socket failure that LOST the lane is the
/// one thing that ends the thread (and, through [`main`]'s shared stop, the
/// process): aterm has gone away and the supervisor should relaunch. A refusal
/// the lane survived is a receipt — see [`lane_survived`] for why that is the
/// test, and for the loss that came of not making it.
fn run_outbox(opts: &Options, stop: &AtomicBool) -> io::Result<()> {
    let mut ctl = connect(opts)?;
    let dir = opts.root.join(".aterm");
    let mut files: BTreeMap<String, OutboxFile> = BTreeMap::new();
    let mut warned = false;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        // ONE OPEN DESCRIPTOR PER MIRRORED SESSION, AND NOT ONE MORE. Each entry
        // holds its session directory open ([`Plane`]), so an entry that outlives
        // its session is a descriptor that never comes back — on a long-lived
        // mirror over an instance that opens and closes sessions, that is EMFILE
        // rather than a leak of a few bytes. Dropping the entry is safe by the
        // module's own argument: everything it held is derived from the files,
        // and re-opening re-seeds the `off=` window from the file's own tail,
        // which is exactly what a mirror restart does.
        let live = match sessions(&mut ctl, opts) {
            Ok(live) => {
                warned = false;
                live
            }
            // As in [`run_inbox`]: an answer the lane survived is retried, not
            // fatal. See [`lane_survived`].
            Err(e) if lane_survived(&ctl) => {
                if !warned {
                    eprintln!("aterm-link mirror: sessions: {e}");
                    warned = true;
                }
                std::thread::sleep(opts.interval);
                continue;
            }
            Err(e) => return Err(e),
        };
        files.retain(|sid, _| live.contains(sid));
        for sid in live {
            let file = match files.entry(sid.clone()) {
                std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::btree_map::Entry::Vacant(e) => {
                    match OutboxFile::open(&dir, &sid) {
                        Ok(f) => e.insert(f),
                        Err(err) => {
                            eprintln!("aterm-link mirror: {sid}: {err}");
                            continue;
                        }
                    }
                }
            };
            for taken in file.take() {
                // THE CURSOR MOVES AFTER THE ACT, ONE LINE AT A TIME, so it
                // means "every line up to here has been sent and answered". The
                // other order — commit the whole batch, then send — turns a
                // crash mid-batch into messages nobody ever sees and nobody can
                // find. This way a crash between the reply and the commit
                // re-sends exactly one line, and `sent.ndjson` shows it twice
                // under the same `at` rather than losing it silently.
                let (end, note) = match taken {
                    Taken::Skip { end } => {
                        file.commit(end);
                        continue;
                    }
                    Taken::TooLong { at, end } => (
                        end,
                        format!(
                            "{{\"at\":{at},\"error\":{}}}",
                            json_string(&format!("line is over {LINE_MAX} bytes; discarded"))
                        ),
                    ),
                    Taken::Line { at, end, text } => {
                        let note = match parse_outbox_line(&text) {
                            Ok(post) => {
                                let (request, body) = post_request(&sid, &post);
                                match scoped(&mut ctl, &request, &body) {
                                    Ok(reply) => format!(
                                        "{{\"at\":{at},\"reply\":{}}}",
                                        json_string(reply.header())
                                    ),
                                    // A LOCAL REFUSAL IS A RECEIPT. Nothing was
                                    // written, the lane is framed and alive, and
                                    // the line that caused it is the agent's own
                                    // — so it is retired with a verdict and the
                                    // cursor moves, exactly as `Taken::TooLong`
                                    // retires an over-long file line.
                                    Err(e) if lane_survived(&ctl) => format!(
                                        "{{\"at\":{at},\"error\":{}}}",
                                        json_string(&e.to_string())
                                    ),
                                    // A LOST LANE ENDS THE PLANE: aterm has gone
                                    // away, and consuming further lines into a
                                    // dead connection would retire each one as an
                                    // error the agent could not tell from a
                                    // refusal.
                                    Err(e) => return Err(e),
                                }
                            }
                            Err(why) => {
                                format!("{{\"at\":{at},\"error\":{}}}", json_string(&why))
                            }
                        };
                        (end, note)
                    }
                };
                if let Err(e) = file.note(&note) {
                    eprintln!("aterm-link mirror: {sid} sent.ndjson: {e}");
                }
                file.commit(end);
            }
        }
        std::thread::sleep(opts.interval);
    }
}

/// What one poll of `outbox.ndjson` found, in file order. Every variant carries
/// the cursor value that retires it, and the caller commits that value only
/// after it has acted — see the loop in [`run_outbox`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Taken {
    /// One complete line, ready to parse and send.
    Line { at: u64, end: u64, text: String },
    /// Bytes with no message in them: a blank line, or the tail of a line
    /// already refused as over-long.
    Skip { end: u64 },
    /// A line longer than [`LINE_MAX`]. It is refused rather than buffered, and
    /// everything up to its newline is discarded — a partial parse of the front
    /// of a huge line would be a message the agent never wrote.
    TooLong { at: u64, end: u64 },
}

/// One session's `outbox.ndjson`, its consumed-bytes cursor, and `sent.ndjson`.
#[derive(Debug)]
pub struct OutboxFile {
    /// The validated, pinned session directory — see [`Plane`].
    plane: Plane,
    path: PathBuf,
    sent: PathBuf,
    cursor: PathBuf,
    /// What `sent.ndjson` was when this process last opened or rotated it.
    sent_id: FileId,
    consumed: u64,
    /// Discarding the remainder of a line that was over [`LINE_MAX`].
    skipping: bool,
}

impl OutboxFile {
    /// Open (creating) one session's outbound files.
    ///
    /// THE CURSOR LIVES IN THE ROOT, with everything else. It is the one piece
    /// of mirror state a sandboxed agent can rewrite — and rewinding it can only
    /// make the mirror re-send that agent's OWN lines, which it could append
    /// again anyway. No authority is stored there, which is what makes keeping
    /// the whole plane inside one directory safe.
    ///
    /// # Errors
    ///
    /// A directory or file that cannot be created, or one that is a symlink.
    pub fn open(aterm_dir: &Path, sid: &str) -> io::Result<Self> {
        let plane = Plane::open(aterm_dir, sid)?;
        let path = plane.path("outbox.ndjson");
        let sent = plane.path("sent.ndjson");
        let cursor = plane.path(".cursor");
        plane.touch(&path)?;
        let sent_id = plane.touch(&sent)?;
        // THROUGH THE WALL, like every other read of this plane. A bare
        // `read_to_string` follows a symlink and cannot see a hard link, so the
        // one file the agent is allowed to rewrite was also the one read with no
        // guard at all — a name pointed anywhere and read for its leading digits.
        // An unreadable or refused cursor is a cursor of 0, exactly as a missing
        // one always was.
        let consumed = plane
            .open_guarded(&cursor, OpenOptions::new().read(true))
            .and_then(|mut f| {
                let mut text = String::new();
                f.read_to_string(&mut text)?;
                Ok(text)
            })
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        Ok(Self {
            plane,
            path,
            sent,
            cursor,
            sent_id,
            consumed,
            skipping: false,
        })
    }

    /// Retire everything up to `end`. MONOTONE, and durable: the cursor is what
    /// stops a restart from re-sending a message the peer already has.
    pub fn commit(&mut self, end: u64) {
        if end <= self.consumed {
            return;
        }
        self.consumed = end;
        let _ = self
            .plane
            .write_durable(&self.cursor, &self.consumed.to_string());
    }

    /// Everything appended since the cursor, in file order.
    ///
    /// A trailing line without its newline is left alone until the newline
    /// arrives: an agent that appends with two `write`s must not have its
    /// message cut in half and sent. Nothing here moves the cursor — see
    /// [`OutboxFile::commit`].
    pub fn take(&mut self) -> Vec<Taken> {
        let Ok(mut f) = self
            .plane
            .open_guarded(&self.path, OpenOptions::new().read(true))
        else {
            return Vec::new();
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.consumed {
            // TRUNCATED OR REPLACED. Start again rather than seeking past the
            // end: the alternative is a cursor that never moves again and an
            // agent whose every later line is silently ignored.
            eprintln!(
                "aterm-link mirror: {} shrank; re-reading from the start",
                self.path.display()
            );
            self.consumed = 0;
            self.skipping = false;
            let _ = self.plane.write_durable(&self.cursor, "0");
        }
        if f.seek(SeekFrom::Start(self.consumed)).is_err() {
            return Vec::new();
        }
        let want = (len - self.consumed).min(READ_MAX);
        let mut buf = vec![0u8; usize::try_from(want).unwrap_or(0)];
        if f.read_exact(&mut buf).is_err() {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut out = Vec::new();
        let mut at = self.consumed;
        let mut skipping = self.skipping;
        for chunk in text.split_inclusive('\n') {
            let end = at + chunk.len() as u64;
            if chunk.ends_with('\n') {
                let line = chunk.trim_end_matches(['\n', '\r']);
                if skipping {
                    skipping = false;
                    out.push(Taken::Skip { end });
                } else if line.trim().is_empty() {
                    out.push(Taken::Skip { end });
                } else {
                    out.push(Taken::Line {
                        at,
                        end,
                        text: line.to_string(),
                    });
                }
            } else if skipping {
                out.push(Taken::Skip { end });
            } else if chunk.len() > LINE_MAX {
                // Not a partial line any more: no legal line is this long, so
                // the rest of it is discarded rather than held in memory until
                // a newline that may never come.
                out.push(Taken::TooLong { at, end });
                skipping = true;
            }
            at = end;
        }
        self.skipping = skipping;
        out
    }

    /// Append one receipt to `sent.ndjson`.
    ///
    /// # Errors
    ///
    /// Any I/O failure.
    pub fn note(&mut self, line: &str) -> io::Result<()> {
        // Same order as the inbound side: the plane is checked before the size
        // is read, because `metadata` and `rename` resolve the same swappable
        // path the open does.
        self.plane.verify()?;
        if std::fs::metadata(&self.sent).is_ok_and(|m| m.len() >= ROTATE_BYTES) {
            let to = self.sent.with_extension("ndjson.1");
            let _ = self.plane.rotate(&self.sent, &to);
            self.sent_id = self.plane.touch(&self.sent)?;
        }
        let mut out = line.to_string();
        out.push('\n');
        self.plane
            .open_tracked(
                &self.sent,
                &mut self.sent_id,
                OpenOptions::new().append(true),
            )?
            .write_all(out.as_bytes())
    }
}

/// One message an agent asked to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Post {
    pub to: String,
    pub kind: String,
    pub re: Option<u64>,
    pub dl: Option<u64>,
    /// The relay chain, VERBATIM as the writer claimed it. Carried onto the wire
    /// and never acted on here: the demotion it forces is the receiving bridge's
    /// (§6.7).
    pub via: Option<String>,
    pub body: Vec<u8>,
}

/// Parse one `outbox.ndjson` line into a post, or say why it is refused.
///
/// EVERY FIELD IS CHECKED AGAINST THE GRAMMAR THE ENDPOINT CHECKS, because this
/// text becomes part of a control-protocol request line. A `to=` holding a space
/// would otherwise end the token and start another; a `kind=` holding a newline
/// would end the request.
///
/// # Errors
///
/// A line that is not a JSON object, a missing or malformed field, a body over
/// [`LINE_MAX`].
pub fn parse_outbox_line(line: &str) -> Result<Post, String> {
    let Some(obj) = json_object(line) else {
        return Err("not one JSON object".to_string());
    };
    let to = obj.get("to").cloned().unwrap_or_default();
    if to == "say" {
        return Err("to=say is not routed by a bridge; post to a session or a principal".into());
    }
    if !valid_to(&to) {
        return Err(format!("to={to:?} is not @<sid>[@<node>] or a principal"));
    }
    let kind = obj.get("kind").cloned().unwrap_or_default();
    if !POSTABLE.contains(&kind.as_str()) {
        return Err(format!(
            "kind={kind:?} is not one of {}",
            POSTABLE.join("|")
        ));
    }
    let number = |key: &str| -> Result<Option<u64>, String> {
        match obj.get(key) {
            None => Ok(None),
            Some(v) => v
                .parse::<u64>()
                .map(Some)
                .map_err(|_| format!("{key}={v:?} is not a number")),
        }
    };
    let re = number("re")?;
    let dl = number("dl")?;
    let via = match obj.get("via") {
        None => None,
        // ONE BOUND FOR THIS FIELD, IN ONE PLACE. [`crate::body::via_ok`] is the
        // shape the endpoint checks plus the two bounds it does not have — the
        // HOP COUNT and the byte length — and the INBOUND side (`bridge`'s
        // `deliver` line) admits a chain by the same function. Every element
        // being a principal bounds an element; it bounds nothing about the LINE,
        // and `post_request` inlines this field into one, so an unbounded chain
        // is a request line the writer must refuse.
        Some(v) if crate::body::via_ok(v) => Some(v.clone()),
        Some(v) => {
            return Err(format!(
                "via={v:?} is not a comma-separated principal list of at most {} hops \
                 ({} bytes)",
                crate::body::VIA_MAX_HOPS,
                crate::body::VIA_MAX_BYTES
            ))
        }
    };
    let body = obj.get("text").cloned().unwrap_or_default().into_bytes();
    if body.is_empty() {
        return Err("text is empty".to_string());
    }
    if body.len() > LINE_MAX {
        return Err(format!("text is {} bytes, over {LINE_MAX}", body.len()));
    }
    Ok(Post {
        to,
        kind,
        re,
        dl,
        via,
        body,
    })
}

/// Whether `to` is an address `post` will take: `@<sid>`, `@<sid>@<node>`, or a
/// bare principal.
fn valid_to(to: &str) -> bool {
    let rest = to.strip_prefix('@').unwrap_or(to);
    match rest.split_once('@') {
        Some((sid, node)) => {
            to.starts_with('@')
                && crate::subject::is_principal(sid)
                && crate::subject::is_principal(node)
        }
        None => crate::subject::is_principal(rest),
    }
}

/// The control request for one post, and the bytes that follow it.
///
/// THE BODY IS A LENGTH-PREFIXED FRAME, never inline text. `post`'s inline form
/// takes the rest of the line, so a body holding a newline would leave its tail
/// to be read as the next verb — the exact shape `post_frame_len`'s own comment
/// in aterm calls out. `len=` makes that impossible by construction, and it
/// raises the ceiling from 4 KiB to 256 KiB on the way.
///
/// `--wait` is NOT passed, and for `ask`/`task` the endpoint turns it on by
/// itself: the outbound thread is the only thing that parks, and the receipt it
/// writes to `sent.ndjson` carries the `off=` an agent needs to recognize the
/// answer that comes back as `re=`.
#[must_use]
pub fn post_request(sid: &str, post: &Post) -> (String, Vec<u8>) {
    let mut line = format!("@{sid} post to={} kind={}", post.to, post.kind);
    if let Some(re) = post.re {
        line.push_str(&format!(" re={re}"));
    }
    if let Some(dl) = post.dl {
        line.push_str(&format!(" dl={dl}"));
    }
    if let Some(via) = &post.via {
        line.push_str(&format!(" via={via}"));
    }
    line.push_str(&format!(" len={}", post.body.len()));
    (line, post.body.clone())
}

// ---------------------------------------------------------------------------
// the `msg` row, and its JSON
// ---------------------------------------------------------------------------

/// One `inbox` `msg` row, parsed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Row {
    pub id: u64,
    /// THE IDENTITY. The broker offset, which is what `deliver` dedups on and
    /// what makes `inbox.ndjson` idempotent.
    pub off: u64,
    pub t: u64,
    pub from: String,
    pub kind: String,
    pub trust: String,
    pub re: Option<u64>,
    pub re_id: Option<u64>,
    pub dl: Option<u64>,
    pub late: bool,
    pub demoted: Option<String>,
    pub via: Option<String>,
    pub len: u64,
    /// The listing truncated the text.
    pub more: bool,
    /// The text as the LISTING carried it, pct-decoded.
    pub text: String,
}

/// Parse one `msg <id> off=<n> …` row. `None` for anything else.
#[must_use]
pub fn parse_msg_row(line: &str) -> Option<Row> {
    let mut toks = line.split(' ').filter(|t| !t.is_empty());
    if toks.next()? != "msg" {
        return None;
    }
    let mut row = Row {
        id: toks.next()?.parse().ok()?,
        ..Row::default()
    };
    for tok in toks {
        let Some((key, value)) = tok.split_once('=') else {
            continue;
        };
        match key {
            "off" => row.off = value.parse().ok()?,
            "t" => row.t = value.parse().unwrap_or(0),
            "from" => row.from = value.to_string(),
            "kind" => row.kind = value.to_string(),
            "trust" => row.trust = value.to_string(),
            "re" => row.re = value.parse().ok(),
            "re-id" => row.re_id = value.parse().ok(),
            "dl" => row.dl = value.parse().ok(),
            "late" => row.late = value == "1",
            "demoted" => row.demoted = Some(value.to_string()),
            "via" => row.via = Some(value.to_string()),
            "len" => row.len = value.parse().unwrap_or(0),
            "more" => row.more = value == "1",
            "text" => row.text = crate::pct::decode(value),
            _ => {}
        }
    }
    (!row.kind.is_empty()).then_some(row)
}

/// One row as the JSON object the file carries. `off` is written first: it is
/// the identity, and a human grepping the file for one message should find it
/// at the front of the line.
///
/// **`"truncated": true` WHEN `text` IS SHORTER THAN `len` SAYS.** This plane's
/// reader is an agent with no socket and no other channel: a body it cannot tell
/// is a fragment is a task it acts on half of. The flag is computed from the
/// RELATION between the bytes written and the size the endpoint declared, so it
/// covers every way a body arrives short — the delivering bridge's own cut of a
/// body too large for a control line, an `inbox get` that failed and left the
/// 512-byte preview, a preview short enough that `more=1` was never set — rather
/// than only the one the caller happens to know about. `glance.rs` sets the same
/// precedent for the same reason: "a short list is not read as a complete one".
#[must_use]
pub fn row_json(row: &Row, text: &str) -> String {
    let mut s = format!("{{\"off\":{}", row.off);
    s.push_str(&format!(",\"id\":{}", row.id));
    s.push_str(&format!(",\"t\":{}", row.t));
    s.push_str(&format!(",\"from\":{}", json_string(&row.from)));
    s.push_str(&format!(",\"kind\":{}", json_string(&row.kind)));
    s.push_str(&format!(",\"trust\":{}", json_string(&row.trust)));
    if let Some(re) = row.re {
        s.push_str(&format!(",\"re\":{re}"));
    }
    if let Some(re_id) = row.re_id {
        s.push_str(&format!(",\"re_id\":{re_id}"));
    }
    if let Some(dl) = row.dl {
        s.push_str(&format!(",\"dl\":{dl}"));
    }
    if row.late {
        s.push_str(",\"late\":true");
    }
    if let Some(d) = &row.demoted {
        s.push_str(&format!(",\"demoted\":{}", json_string(d)));
    }
    if let Some(v) = &row.via {
        s.push_str(&format!(",\"via\":{}", json_string(v)));
    }
    s.push_str(&format!(",\"len\":{}", row.len));
    if (text.len() as u64) < row.len {
        s.push_str(",\"truncated\":true");
    }
    s.push_str(&format!(",\"text\":{}}}", json_string(text)));
    s
}

// ---------------------------------------------------------------------------
// the smallest JSON that does this job
// ---------------------------------------------------------------------------

/// Render one string as a JSON string literal, control characters escaped.
#[must_use]
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parse a FLAT JSON object into `key -> value as text`: a string becomes its
/// decoded contents, a number or `true`/`false`/`null` its literal text, and a
/// nested object or array is accepted and discarded.
///
/// TOTAL over arbitrary input: it never panics, it never recurses past
/// [`DEPTH_MAX`], and anything it cannot make sense of is `None` rather than a
/// half-parsed object — because every caller is holding bytes a sandboxed
/// process wrote.
#[must_use]
pub fn json_object(src: &str) -> Option<BTreeMap<String, String>> {
    /// The nesting an ignored value may carry before the line is refused.
    const DEPTH_MAX: usize = 16;
    let b = src.as_bytes();
    let mut i = 0usize;
    skip_ws(b, &mut i);
    if b.get(i) != Some(&b'{') {
        return None;
    }
    i += 1;
    let mut out = BTreeMap::new();
    skip_ws(b, &mut i);
    if b.get(i) == Some(&b'}') {
        i += 1;
        skip_ws(b, &mut i);
        return (i == b.len()).then_some(out);
    }
    loop {
        skip_ws(b, &mut i);
        let key = json_str_at(b, &mut i)?;
        skip_ws(b, &mut i);
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        skip_ws(b, &mut i);
        let value = json_value_at(b, &mut i, DEPTH_MAX)?;
        out.insert(key, value);
        skip_ws(b, &mut i);
        match b.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => {
                i += 1;
                break;
            }
            _ => return None,
        }
    }
    skip_ws(b, &mut i);
    (i == b.len()).then_some(out)
}

fn skip_ws(b: &[u8], i: &mut usize) {
    while matches!(b.get(*i), Some(b' ' | b'\t' | b'\r' | b'\n')) {
        *i += 1;
    }
}

/// One JSON string starting at `b[*i]`, decoded.
fn json_str_at(b: &[u8], i: &mut usize) -> Option<String> {
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let mut out = String::new();
    let mut pending_high: Option<u16> = None;
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        match c {
            b'"' => {
                if pending_high.is_some() {
                    out.push('\u{fffd}');
                }
                return Some(out);
            }
            b'\\' => {
                let esc = *b.get(*i)?;
                *i += 1;
                let plain = match esc {
                    b'"' => Some('"'),
                    b'\\' => Some('\\'),
                    b'/' => Some('/'),
                    b'b' => Some('\u{8}'),
                    b'f' => Some('\u{c}'),
                    b'n' => Some('\n'),
                    b'r' => Some('\r'),
                    b't' => Some('\t'),
                    b'u' => None,
                    _ => return None,
                };
                match plain {
                    Some(ch) => {
                        if pending_high.take().is_some() {
                            out.push('\u{fffd}');
                        }
                        out.push(ch);
                    }
                    None => {
                        let unit = hex4(b, i)?;
                        // Surrogate pairs, because a JSON writer that escapes
                        // everything writes astral characters this way and a
                        // reader that dropped them would silently mangle text.
                        match (pending_high.take(), unit) {
                            (Some(hi), 0xDC00..=0xDFFF) => {
                                let cp = 0x1_0000
                                    + ((u32::from(hi) - 0xD800) << 10)
                                    + (u32::from(unit) - 0xDC00);
                                out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                            }
                            (prev, _) => {
                                if prev.is_some() {
                                    out.push('\u{fffd}');
                                }
                                if (0xD800..=0xDBFF).contains(&unit) {
                                    pending_high = Some(unit);
                                } else if (0xDC00..=0xDFFF).contains(&unit) {
                                    out.push('\u{fffd}');
                                } else {
                                    out.push(char::from_u32(u32::from(unit)).unwrap_or('\u{fffd}'));
                                }
                            }
                        }
                    }
                }
            }
            // A raw control character is not legal in a JSON string, and
            // accepting one here is how a newline would reach a request line.
            0x00..=0x1f => return None,
            _ => {
                if pending_high.take().is_some() {
                    out.push('\u{fffd}');
                }
                // Re-decode the UTF-8 sequence this byte starts.
                let start = *i - 1;
                let mut end = *i;
                while end < b.len() && (b[end] & 0xC0) == 0x80 {
                    end += 1;
                }
                out.push_str(&String::from_utf8_lossy(&b[start..end]));
                *i = end;
            }
        }
    }
}

fn hex4(b: &[u8], i: &mut usize) -> Option<u16> {
    let mut v: u16 = 0;
    for _ in 0..4 {
        let d = (*b.get(*i)? as char).to_digit(16)?;
        v = v.checked_mul(16)?.checked_add(u16::try_from(d).ok()?)?;
        *i += 1;
    }
    Some(v)
}

/// One JSON value, as text. Containers are consumed and answered as `""`.
fn json_value_at(b: &[u8], i: &mut usize, depth: usize) -> Option<String> {
    match b.get(*i)? {
        b'"' => json_str_at(b, i),
        b'{' | b'[' => {
            if depth == 0 {
                return None;
            }
            skip_container(b, i, depth)?;
            Some(String::new())
        }
        b't' | b'f' | b'n' => {
            for word in ["true", "false", "null"] {
                if b[*i..].starts_with(word.as_bytes()) {
                    *i += word.len();
                    return Some(word.to_string());
                }
            }
            None
        }
        _ => {
            let start = *i;
            while matches!(b.get(*i), Some(c) if c.is_ascii_digit()
                || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E'))
            {
                *i += 1;
            }
            (start != *i).then(|| String::from_utf8_lossy(&b[start..*i]).into_owned())
        }
    }
}

/// Consume one object or array without keeping it.
fn skip_container(b: &[u8], i: &mut usize, depth: usize) -> Option<()> {
    let close = if b.get(*i) == Some(&b'{') { b'}' } else { b']' };
    *i += 1;
    loop {
        skip_ws(b, i);
        if b.get(*i)? == &close {
            *i += 1;
            return Some(());
        }
        if b.get(*i) == Some(&b',') || b.get(*i) == Some(&b':') {
            *i += 1;
            continue;
        }
        json_value_at(b, i, depth - 1)?;
    }
}

// ---------------------------------------------------------------------------
// files, opened as if the other end were hostile — because it is
// ---------------------------------------------------------------------------

/// Create a directory, refusing one that is (or is behind) a symlink.
fn ensure_dir(path: &Path) -> io::Result<()> {
    if let Ok(md) = std::fs::symlink_metadata(path) {
        if md.file_type().is_symlink() {
            return Err(symlink_refusal(path));
        }
        if md.is_dir() {
            return Ok(());
        }
    }
    std::fs::create_dir_all(path)
}

/// What `fstat` says a file IS, as opposed to what a path says it is.
type FileId = (u64, u64);

/// The `(device, inode)` of an OPEN descriptor — read from the descriptor, so no
/// path swap can change the answer.
fn file_id(f: &File) -> io::Result<FileId> {
    use std::os::unix::fs::MetadataExt;
    let md = f.metadata()?;
    Ok((md.dev(), md.ino()))
}

/// **THE WALL**: one session's mirror directory, validated once and PINNED, with
/// every later operation re-checked against the pin.
///
/// The two components this process owns — `<root>/.aterm` and
/// `<root>/.aterm/<sid>` — are created and refused-if-a-symlink at open. That
/// used to be the whole of it, and it ran exactly once: every later append,
/// rotation, receipt and cursor write re-resolved the FULL path with only the
/// leaf guarded, because `symlink_metadata` resolves the parents before it
/// stats the leaf and `O_NOFOLLOW` binds the final component and nothing else.
/// Under this module's own threat model — THE ROOT IS HOSTILE GROUND, and the
/// mirror runs with the operator's authority — that is a check the adversary
/// invalidates at leisure: `mv <sid> keep && ln -s /elsewhere <sid>`, and from
/// then on a peer's message text is appended OUTSIDE the sandbox, `.cursor` is
/// created and renamed outside it, and the outbox lines that get posted to the
/// fabric are read from outside it.
///
/// So the validated directory is OPENED and held for the life of the process,
/// and every operation below re-resolves the path and compares what it finds
/// against `fstat` of that descriptor. The held descriptor is what makes the
/// comparison unspoofable: it names the directory this process validated, even
/// after the PATH has been made to name a different one. One descriptor per
/// mirrored session, which is the same bound as the sessions themselves.
///
/// ## Re-verify, not `openat`, and why
///
/// Opening every file RELATIVE to the held descriptor (`openat`, `renameat`) is
/// the construction that leaves no window at all, and it is what this should
/// become. It needs either new `unsafe` in a module that has none — aterm's
/// doctrine keeps `unsafe` in cordoned crates, and `aterm-uds` is the one that
/// owns descriptors — or a dependency this crate does not have (`aterm-dirfd`,
/// one workspace over, is exactly this primitive and exists for exactly this
/// property). Neither is A9's to take, so what is built here is the strongest
/// thing that is: the swap is caught before every operation, and what remains is
/// the window between that check and the syscall on the next line.
///
/// For the two paths that WRITE attacker-visible content — `inbox.ndjson` and
/// `sent.ndjson` — that window is narrowed further by [`Plane::open_tracked`],
/// which checks the descriptor it actually opened against the inode this process
/// pinned, so a swap has to be invisible at two independent checks with an open
/// between them rather than win one race.
///
/// ## The leaf, and why a symlink guard was not one
///
/// The directory pin says the leaf is in the right DIRECTORY. It says nothing
/// about what the leaf IS, and the leaf guard used to be two symlink checks —
/// which a hard link walks straight past: `lstat` reports `S_ISLNK == false`,
/// `O_NOFOLLOW` binds only a symbolic final component, and the inode never
/// changes, so [`Plane::open_tracked`]'s second look matches the first. `ln
/// ~/.ssh/authorized_keys <root>/.aterm/<sid>/inbox.ndjson`, planted at any time
/// including before the mirror attaches, and every delivered row — a peer's
/// bytes, written verbatim — is appended to that file with the OPERATOR's uid.
/// The same link on `sent.ndjson` writes receipts there; on `outbox.ndjson` it
/// reads a file outside the root and posts it to the fabric.
///
/// [`Plane::leaf_is_private`] is therefore the guard, and it is a statement
/// about the four files rather than about one attack: each is a REGULAR file
/// that only this process and the agent create, in a directory only they use,
/// so `st_nlink == 1` is not a heuristic — it is the invariant, and anything
/// else at that name (a second link, a fifo whose `open` would hang the mirror,
/// a device) is somebody reaching for something. It is checked on the PATH
/// before the open, so a truncating or blocking open never happens, and again on
/// the DESCRIPTOR after it, where no path swap can change the answer.
///
/// What remains is what the pin's window always was: the gap between the
/// path-side check and the syscall on the next line, for the opens that do not
/// also carry `O_EXCL`. [`Plane::write_durable`] does carry it — its temp is
/// unlinked and then created `create_new`, because that is the one open that
/// TRUNCATES and a truncate lands before any check of ours could run. `openat`
/// on the held descriptor closes the rest, and is what this becomes when
/// `aterm-dirfd` is a dependency this crate may take.
#[derive(Debug)]
struct Plane {
    /// `<root>/.aterm/<sid>`.
    dir: PathBuf,
    /// The validated directory, held open. Never read from; it exists to be
    /// `fstat`ed.
    held: File,
}

impl Plane {
    /// Create and validate `<aterm_dir>/<sid>`, then pin it.
    fn open(aterm_dir: &Path, sid: &str) -> io::Result<Self> {
        ensure_dir(aterm_dir)?;
        let dir = aterm_dir.join(sid);
        ensure_dir(&dir)?;
        let held = OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW)
            .open(&dir)?;
        if !held.metadata()?.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotADirectory,
                format!("{} is not a directory", dir.display()),
            ));
        }
        Ok(Self { dir, held })
    }

    /// A path inside this plane. Nothing else may build one.
    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    /// Refuse unless the path still names the directory this process validated.
    fn verify(&self) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let pinned = self.held.metadata()?;
        // `symlink_metadata`, so a `<sid>` that has BECOME a symlink reports the
        // link's own inode and fails the comparison rather than being followed.
        let now = std::fs::symlink_metadata(&self.dir)?;
        if now.is_dir() && (now.dev(), now.ino()) == (pinned.dev(), pinned.ino()) {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is no longer the directory this mirror validated; refusing to \
                 write outside the sandbox root",
                self.dir.display()
            ),
        ))
    }

    /// Open one file of this plane: the directory pin, then the leaf's own
    /// guards — [`Plane::leaf_is_private`] on the path (a readable refusal, and
    /// the only chance to refuse before a truncating or blocking `open`),
    /// `O_NOFOLLOW` for the window between that and the open, and
    /// [`Plane::leaf_is_private`] again on the descriptor, which no path swap
    /// can change the answer to.
    fn open_guarded(&self, path: &Path, opts: &mut OpenOptions) -> io::Result<File> {
        if path.parent() != Some(self.dir.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not in this mirror plane", path.display()),
            ));
        }
        self.verify()?;
        if let Ok(md) = std::fs::symlink_metadata(path) {
            Self::leaf_is_private(path, &md)?;
        }
        let f = opts.custom_flags(O_NOFOLLOW).open(path)?;
        // ON THE DESCRIPTOR, not on the name. The check above can be raced; this
        // one is `fstat` of the thing actually opened, and it runs before the
        // caller has written a byte.
        Self::leaf_is_private(path, &f.metadata()?)?;
        Ok(f)
    }

    /// Whether a leaf is the private regular file this plane's four names are,
    /// or something somebody put there to be written through.
    ///
    /// `st_nlink == 1` IS THE INVARIANT, not a heuristic: `inbox.ndjson`,
    /// `inbox.ndjson.1`, `sent.ndjson`, `outbox.ndjson` and `.cursor` are
    /// created by this process or by the agent inside a directory only the two
    /// of them use, and nothing legitimate ever gives one of them a second name.
    /// A second name is the whole attack — it is a symlink that `lstat` calls a
    /// file and `O_NOFOLLOW` cannot see — and it is the same escape the symlink
    /// guard exists to refuse, reached through the other kind of link.
    ///
    /// The regular-file half is the same argument for the other things a name
    /// can be: `open`ing a fifo the agent planted would block the mirror for
    /// every session it serves, and a device is not a place to append JSON.
    fn leaf_is_private(path: &Path, md: &std::fs::Metadata) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        if md.file_type().is_symlink() {
            return Err(symlink_refusal(path));
        }
        if !md.is_file() {
            return Err(leaf_refusal(path, "is not a regular file"));
        }
        if md.nlink() != 1 {
            return Err(leaf_refusal(
                path,
                &format!(
                    "has {} links, so the name is shared with a file this \
                     mirror did not create",
                    md.nlink()
                ),
            ));
        }
        Ok(())
    }

    /// Open a file that should still be the one `id` names, updating `id` when
    /// the agent has legitimately replaced its own file.
    ///
    /// A mismatch is NEVER written to: the descriptor that caused it is dropped
    /// unread, the directory is checked again from the held fd, and the file is
    /// opened a second time. An agent replacing its own `inbox.ndjson` (its
    /// right — it owns the root) costs one extra open; an escape has to be
    /// invisible at two independent directory checks with an open between them.
    fn open_tracked(
        &self,
        path: &Path,
        id: &mut FileId,
        opts: &mut OpenOptions,
    ) -> io::Result<File> {
        let f = self.open_guarded(path, opts)?;
        if file_id(&f)? == *id {
            return Ok(f);
        }
        drop(f);
        let f = self.open_guarded(path, opts)?;
        *id = file_id(&f)?;
        Ok(f)
    }

    /// Create the file if it is not there, and pin what is there now. Never
    /// truncates: the file IS the state.
    fn touch(&self, path: &Path) -> io::Result<FileId> {
        if std::fs::symlink_metadata(path).is_ok() {
            // Present already — but if it is a symlink, refuse now rather than
            // at the first append.
            return file_id(&self.open_guarded(path, OpenOptions::new().read(true))?);
        }
        file_id(&self.open_guarded(path, OpenOptions::new().create(true).append(true))?)
    }

    /// Rotate `path` to `to`, both inside the plane.
    fn rotate(&self, path: &Path, to: &Path) -> io::Result<()> {
        self.verify()?;
        if path.parent() != Some(self.dir.as_path()) || to.parent() != Some(self.dir.as_path()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a rotation must stay inside the plane",
            ));
        }
        std::fs::rename(path, to)
    }

    /// Write one small file durably: temp, fsync, rename. Both halves are
    /// checked, because the rename is as much a way out of the root as the open.
    fn write_durable(&self, path: &Path, value: &str) -> io::Result<()> {
        let tmp = path.with_extension("tmp");
        // UNLINK, THEN `O_CREAT|O_EXCL` — never `O_TRUNC`. This is the only open
        // in the plane that destroys what it finds, and it destroys it INSIDE
        // `open(2)`, before any check of ours could refuse the leaf; a hard link
        // planted at `.cursor.tmp` would therefore have emptied the linked-to
        // file even though nothing was ever written through it. `remove_file` is
        // `unlink`, so it removes the NAME and never follows it, and `create_new`
        // then refuses anything that raced back into it.
        self.verify()?;
        match std::fs::remove_file(&tmp) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        {
            let mut f = self.open_guarded(&tmp, OpenOptions::new().write(true).create_new(true))?;
            f.write_all(value.as_bytes())?;
            f.write_all(b"\n")?;
            f.sync_all()?;
        }
        self.verify()?;
        std::fs::rename(&tmp, path)
    }

    /// The last `max_bytes` of a file, as whole lines. A partial first line is
    /// dropped, and a file that is not UTF-8 decodes lossily rather than failing
    /// — the agent can put anything in there.
    fn tail_lines(&self, path: &Path, max_bytes: u64) -> Vec<String> {
        let Ok(mut f) = self.open_guarded(path, OpenOptions::new().read(true)) else {
            return Vec::new();
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let from = len.saturating_sub(max_bytes);
        if f.seek(SeekFrom::Start(from)).is_err() {
            return Vec::new();
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            return Vec::new();
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
        if from > 0 && !lines.is_empty() {
            lines.remove(0);
        }
        lines
    }
}

/// A leaf that is not the private regular file this plane's names are. Same
/// shape and same kind as [`symlink_refusal`]: they refuse the same escape.
fn leaf_refusal(path: &Path, why: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{} {why}; the mirror writes only files it created inside its root",
            path.display()
        ),
    )
}

fn symlink_refusal(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{} is a symlink; the mirror never follows one out of its root",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("atlink-mirror-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("scratch");
        p
    }

    fn row(off: u64, id: u64) -> Row {
        Row {
            id,
            off,
            kind: "note".into(),
            trust: "agent".into(),
            from: "s-a@n-b".into(),
            len: 2,
            ..Row::default()
        }
    }

    /// A pair of connected sockets, one end wrapped as a [`Ctl`]. No handshake
    /// and no server: what is under test is what this module does BEFORE a byte
    /// moves, and the peer end is here to prove that no byte did.
    ///
    /// The read timeout is a HANG DETECTOR and never a synchronisation: on the
    /// code under test nothing is written, so nothing is ever read and the
    /// timeout cannot elapse. It is here because the FAILING version of this
    /// test does write — and a regression test that hangs instead of failing
    /// tells nobody anything.
    fn pair() -> (Ctl, std::os::unix::net::UnixStream) {
        let (mine, theirs) = std::os::unix::net::UnixStream::pair().expect("a socketpair");
        theirs.set_nonblocking(true).expect("nonblocking peer");
        let ctl = Ctl::from_stream(mine).expect("wrap one end");
        ctl.get_ref()
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("a hang detector on the reply read");
        (ctl, theirs)
    }

    /// Whether anything at all has been written to the peer end.
    fn peer_saw_bytes(peer: &mut std::os::unix::net::UnixStream) -> bool {
        let mut buf = [0u8; 1];
        !matches!(peer.read(&mut buf), Err(ref e) if e.kind() == io::ErrorKind::WouldBlock)
    }

    /// **THE CONTAINMENT CLAIM IS A MECHANISM, NOT A SENTENCE.** The module
    /// header says this plane never calls a `BridgeOnly` verb — the whole of
    /// §8.5's confused-deputy argument for the one plane a contained agent
    /// writes into. The set is read from the VERB TABLE, so a verb reclassified
    /// tomorrow is covered by this test today, and the refusal is local: the
    /// lane is untouched and no byte is written.
    #[test]
    fn the_mirror_names_no_bridge_only_verb() {
        let bridge_only: Vec<&str> = aterm_types::control_verbs::VERBS
            .iter()
            .filter(|spec| aterm_types::control_verbs::is_bridge_only(spec.name))
            .map(|spec| spec.name)
            .collect();
        assert!(
            bridge_only.len() >= 2,
            "the table must still HAVE a BridgeOnly set, or this test guards nothing"
        );
        let (mut ctl, mut peer) = pair();
        for verb in bridge_only {
            for line in [
                format!("@s-one {verb} off=1 from=h-a"),
                format!("{verb} off=1"),
            ] {
                let err = scoped(&mut ctl, &line, &[]).expect_err(line.as_str());
                assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{line}");
                assert!(
                    lane_survived(&ctl),
                    "a local refusal must not latch the lane: {line}"
                );
                assert!(!peer_saw_bytes(&mut peer), "nothing may be written: {line}");
            }
        }
        // And the verbs it DOES speak are not refused by the guard. (They fail
        // on the reply, because nobody is answering — that is the next test.)
        assert!(bridge_only_verb("@s-one inbox --peek since=3").is_none());
        assert!(bridge_only_verb("@s-one post to=@s-two kind=note len=1").is_none());
        assert!(bridge_only_verb("sessions").is_none());

        // AND THE SENTENCE ITSELF. An R1 edit deleted the two words that carried
        // the negation, leaving "…calls a `BridgeOnly` verb, so the sandbox-facing
        // plane cannot reach the bridge plane" — the inverse of the property, in
        // the one place a reader auditing §8.5 looks first, in a repo with no
        // evidence manifest where a doc comment IS the claim. Nothing pinned it.
        let src = include_str!("mirror.rs");
        let header: String = src
            .lines()
            .take_while(|l| l.starts_with("//") || l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            header.contains("never calls a `BridgeOnly` verb"),
            "the module header must still MAKE the containment claim it is the \
             designated place for"
        );
    }

    /// **THE HELP SAYS WHAT THE DEFAULT COSTS.** `Options::sessions` documents
    /// the hazard — no `--session` mirrors EVERY session, and the mirror moves
    /// `seen=` on the agent's behalf, so mirroring a session that has its own
    /// socket client marks mail delivered that nobody read. The operator-facing
    /// `USAGE` said only "mirror only this session; repeatable", so an operator
    /// running `--help` could not learn that the default is the configuration
    /// the module says not to use.
    #[test]
    fn the_help_states_the_default_and_what_it_costs() {
        assert!(
            USAGE.contains("EVERY session"),
            "the help must state the default: {USAGE}"
        );
        assert!(
            USAGE.contains("inbox seen"),
            "the help must state the watermark cost: {USAGE}"
        );
    }

    /// **A REFUSAL THE LANE SURVIVED IS NOT A LOST LANE.** `ctl.rs` states the
    /// distinction as its own discipline and this module ignored it: every `Ctl`
    /// error was propagated with `?`, so one over-long request line — which the
    /// writer refuses locally, having written NOTHING — ended the outbound plane
    /// for the life of the process. Only a lost lane means "aterm has gone
    /// away".
    #[test]
    fn a_local_refusal_is_not_a_lost_lane() {
        let (mut ctl, mut peer) = pair();
        let too_long = format!("@s-one post to=@s-two kind=note x={}", "a".repeat(70_000));
        let err = scoped(&mut ctl, &too_long, &[]).expect_err("over the writer's bound");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            lane_survived(&ctl),
            "nothing was written, so nothing was lost"
        );
        assert!(!peer_saw_bytes(&mut peer));

        // AND A LOST ONE IS. The peer goes away; the next request fails on I/O,
        // the latch closes, and `lane_survived` says so — which is what ends the
        // plane and lets a supervisor relaunch.
        drop(peer);
        let _ = scoped(&mut ctl, "sessions", &[]);
        assert!(!lane_survived(&ctl), "a dead peer is a lost lane");
    }

    /// **NO LINE THIS MODULE CAN BUILD REACHES THE WRITER'S BOUND.** `via` was
    /// checked element by element and never counted, and `post_request` inlines
    /// the whole chain — three components with three implicit limits for one
    /// value, which is this project's signature defect. The widest post the
    /// grammar admits is measured against the one constant that decides.
    #[test]
    fn no_post_line_this_module_builds_can_reach_the_writers_bound() {
        // The widest NAME this module's own grammar admits — `subject::is_principal`
        // is stricter than the endpoint's, and the line is built from this one.
        let name = "a".repeat(crate::subject::PRINCIPAL_MAX - "n-".len());
        let hop = format!("n-{name}");
        let widest = Post {
            to: format!("@s-{name}@n-{name}"),
            kind: "control".into(),
            re: Some(u64::MAX),
            dl: Some(u64::MAX),
            via: Some(vec![hop.clone(); crate::body::VIA_MAX_HOPS].join(",")),
            body: vec![b'x'; LINE_MAX],
        };
        let (line, body) = post_request(&format!("s-{name}"), &widest);
        assert!(
            line.len() <= crate::ctl::REQUEST_LINE_MAX,
            "the widest line the grammar admits is {} bytes",
            line.len()
        );
        assert_eq!(
            body.len(),
            LINE_MAX,
            "the body travels as a frame, not a line"
        );

        // The chain that used to be accepted is refused, with a reason, and the
        // reason reaches the agent as a receipt rather than as a dead plane.
        let chain = vec![hop; 2000].join(",");
        let line =
            format!("{{\"to\":\"@s-two\",\"kind\":\"note\",\"via\":\"{chain}\",\"text\":\"x\"}}");
        let why = parse_outbox_line(&line).expect_err("2000 hops is not a relay chain");
        assert!(why.contains("hop"), "{why}");
        // One under the bound still parses: the bound is a bound, not a ban.
        let ok = format!(
            "{{\"to\":\"@s-two\",\"kind\":\"note\",\"via\":\"{}\",\"text\":\"x\"}}",
            vec![format!("n-{name}"); crate::body::VIA_MAX_HOPS].join(",")
        );
        assert!(parse_outbox_line(&ok).is_ok());
    }

    /// **A FRAGMENT SAYS SO.** This plane's reader has no socket and no other
    /// channel, so a body short of its own `len=` that is not marked is a task
    /// an agent acts on half of. The flag is on the RELATION between the bytes
    /// written and the declared size, so it covers every way a body arrives
    /// short — the delivering bridge's own cut, a failed `inbox get`, a preview
    /// too short for `more=1` — and not only the case the caller knows about.
    #[test]
    fn a_body_shorter_than_its_len_is_marked_truncated() {
        let mut r = row(7, 1);
        r.len = 200_000;
        let cut = row_json(&r, "the first 46 KiB");
        assert!(cut.contains("\"truncated\":true"), "{cut}");
        assert!(cut.contains("\"len\":200000"), "{cut}");
        // Whole bodies are not marked, and neither is an empty one whose `len`
        // agrees with it.
        let mut whole = row(7, 1);
        whole.len = 2;
        assert!(!row_json(&whole, "hi").contains("truncated"));
        let mut empty = row(7, 1);
        empty.len = 0;
        assert!(!row_json(&empty, "").contains("truncated"));
        // A multi-byte body is measured in BYTES, the unit `len=` is in.
        let mut wide = row(7, 1);
        wide.len = 4;
        assert!(
            !row_json(&wide, "éé").contains("truncated"),
            "two chars are four BYTES, and `len=` counts bytes"
        );
        wide.len = 5;
        assert!(
            row_json(&wide, "éé").contains("truncated"),
            "one byte short"
        );
    }

    /// **`inbox.ndjson` is `off=`-idempotent, across a restart.** The file is
    /// the state: a fresh [`InboxFile`] over the same path must recognize every
    /// offset already in it, because a mirror that came back and re-listed the
    /// ring is exactly what happens after aterm relaunches.
    #[test]
    fn the_inbox_file_is_off_idempotent_across_a_reopen() {
        let dir = scratch("idem");
        let mut f = InboxFile::open(&dir, "s-one").expect("open");
        f.append(&row(10, 1), "hi").expect("append");
        f.append(&row(10, 1), "hi")
            .expect("the same offset again is a no-op");
        assert!(f.holds(10));
        f.append(&row(11, 2), "ho").expect("append");

        let mut again = InboxFile::open(&dir, "s-one").expect("reopen");
        assert!(again.holds(10) && again.holds(11), "seeded from the file");
        assert!(!again.holds(12));
        again.append(&row(12, 3), "he").expect("append");

        let body = std::fs::read_to_string(dir.join("s-one/inbox.ndjson")).expect("read");
        let offs: Vec<String> = body
            .lines()
            .filter_map(|l| json_object(l).and_then(|m| m.get("off").cloned()))
            .collect();
        assert_eq!(offs, ["10", "11", "12"], "one line per offset:\n{body}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The append writes ONE line per row and every row parses back as JSON,
    /// text included — a mirror whose own output its reader cannot parse is a
    /// mirror of nothing.
    #[test]
    fn a_written_row_parses_back_with_its_fields() {
        let dir = scratch("json");
        let mut f = InboxFile::open(&dir, "s-two").expect("open");
        let mut r = row(90_312, 41);
        r.kind = "note".into();
        r.trust = "relayed".into();
        r.demoted = Some("task".into());
        r.via = Some("s-inner01".into());
        r.re = Some(90_300);
        f.append(&r, "line one\nline \"two\"\ttab").expect("append");
        let body = std::fs::read_to_string(dir.join("s-two/inbox.ndjson")).expect("read");
        assert_eq!(
            body.lines().count(),
            1,
            "a newline in the text split the row"
        );
        let obj = json_object(body.trim()).expect("valid JSON");
        assert_eq!(obj.get("off").map(String::as_str), Some("90312"));
        assert_eq!(obj.get("demoted").map(String::as_str), Some("task"));
        assert_eq!(obj.get("via").map(String::as_str), Some("s-inner01"));
        assert_eq!(obj.get("re").map(String::as_str), Some("90300"));
        assert_eq!(
            obj.get("text").map(String::as_str),
            Some("line one\nline \"two\"\ttab")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every complete line a poll found, in order, as text — for the tests
    /// below, which care about WHICH lines were taken and not about the byte
    /// bookkeeping under them.
    fn drain(f: &mut OutboxFile) -> Vec<String> {
        let mut out = Vec::new();
        for taken in f.take() {
            match taken {
                Taken::Line { end, text, .. } => {
                    out.push(text);
                    f.commit(end);
                }
                Taken::Skip { end } | Taken::TooLong { end, .. } => f.commit(end),
            }
        }
        out
    }

    /// **A partial trailing line is not sent.** An agent that appends with two
    /// writes must not have half a message posted, and the byte cursor must not
    /// step over the half it did not take.
    #[test]
    fn a_partial_outbox_line_waits_for_its_newline() {
        let dir = scratch("partial");
        let mut f = OutboxFile::open(&dir, "s-three").expect("open");
        let path = dir.join("s-three/outbox.ndjson");
        let one = "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"one\"}";
        let two = "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"two\"}";
        std::fs::write(&path, format!("{one}\n{{\"to\"")).expect("write");
        assert_eq!(drain(&mut f), [one], "only the complete line");
        std::fs::write(&path, format!("{one}\n{two}\n")).expect("append the rest");
        assert_eq!(drain(&mut f), [two], "the first line is not re-taken");
        // The cursor survives a reopen: a restart must not re-post everything.
        let mut again = OutboxFile::open(&dir, "s-three").expect("reopen");
        assert!(drain(&mut again).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **THE CURSOR MOVES AFTER THE ACT.** `take` promises nothing about the
    /// cursor; a caller that never commits gets the same line again, which is
    /// what makes "sent and answered" the meaning of the stored offset rather
    /// than "read".
    #[test]
    fn a_line_that_is_never_committed_is_offered_again() {
        let dir = scratch("nocommit");
        let mut f = OutboxFile::open(&dir, "s-six").expect("open");
        let path = dir.join("s-six/outbox.ndjson");
        std::fs::write(
            &path,
            "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"x\"}\n",
        )
        .expect("write");
        let first = f.take();
        assert_eq!(first.len(), 1);
        assert_eq!(f.take(), first, "uncommitted, so offered again");
        let Taken::Line { end, .. } = first[0].clone() else {
            panic!("a complete line")
        };
        f.commit(end);
        assert!(f.take().is_empty(), "committed, so retired");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A line no legal message could be is refused, and the rest of it is
    /// discarded** — not buffered until a newline that may never come, and not
    /// parsed from its middle as if the tail were a line of its own.
    #[test]
    fn an_over_long_outbox_line_is_refused_and_its_tail_discarded() {
        let dir = scratch("toolong");
        let mut f = OutboxFile::open(&dir, "s-seven").expect("open");
        let path = dir.join("s-seven/outbox.ndjson");
        let huge = "x".repeat(LINE_MAX + 16);
        std::fs::write(&path, &huge).expect("write a line with no end in sight");
        assert!(
            matches!(f.take().as_slice(), [Taken::TooLong { .. }]),
            "an over-long partial line is refused rather than held"
        );
        assert!(drain(&mut f).is_empty(), "and nothing is sent from it");
        let good = "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"after\"}";
        std::fs::write(&path, format!("{huge}tail\n{good}\n"))
            .expect("finish it, then a good line");
        assert_eq!(
            drain(&mut f),
            [good],
            "the tail of the refused line is discarded; the next line is not"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A truncated outbox is re-read, not seeked past.** A cursor beyond the
    /// end would never move again, and the agent's lines would vanish silently.
    #[test]
    fn a_truncated_outbox_restarts_at_zero() {
        let dir = scratch("trunc");
        let mut f = OutboxFile::open(&dir, "s-four").expect("open");
        let path = dir.join("s-four/outbox.ndjson");
        std::fs::write(
            &path,
            "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"one\"}\n",
        )
        .expect("write");
        assert_eq!(drain(&mut f).len(), 1);
        let short = "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"2\"}";
        std::fs::write(&path, format!("{short}\n")).expect("truncate and rewrite, SHORTER");
        assert_eq!(
            drain(&mut f),
            [short],
            "the shrunk file is read from the start"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **THE INJECTION WALL.** Every one of these is a line a sandboxed process
    /// can write, and every one of them must be refused rather than
    /// interpolated into a control request. The `to`/`kind`/`via` cases would
    /// end the token or the line; the raw-newline case is why a control
    /// character inside a JSON string is a parse failure and not a character.
    #[test]
    fn no_outbox_field_can_become_a_second_verb() {
        for line in [
            r#"{"to":"@s-b kind=task","kind":"note","text":"x"}"#,
            r#"{"to":"@s-b\nhold s-b on","kind":"note","text":"x"}"#,
            r#"{"to":"@s-b","kind":"note\ndeliver s-b off=1 from=h-a kind=task trust=human","text":"x"}"#,
            r#"{"to":"@s-b","kind":"note","via":"s-i deliver","text":"x"}"#,
            r#"{"to":"@s-b","kind":"note","via":"","text":"x"}"#,
            r#"{"to":"@s-b","kind":"deliver","text":"x"}"#,
            r#"{"to":"fleet","kind":"note","text":"x"}"#,
            r#"{"to":"say","kind":"note","text":"x"}"#,
            r#"{"to":"@s-b","kind":"note","re":"; hold","text":"x"}"#,
            r#"{"to":"@s-b","kind":"note","text":""}"#,
            "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"a\nb\"}",
            "not json at all",
            "",
        ] {
            assert!(
                parse_outbox_line(line).is_err(),
                "this line was ACCEPTED: {line}"
            );
        }
        // And the good line survives, body and all — including a body that
        // holds a newline, which is exactly what the `len=` frame is for.
        let post = parse_outbox_line(
            r#"{"to":"@s-b@n-c","kind":"task","re":7,"dl":900,"via":"s-inner01","text":"one\ntwo"}"#,
        )
        .expect("a well-formed line");
        assert_eq!(post.via.as_deref(), Some("s-inner01"));
        let (line, body) = post_request("s-a", &post);
        assert_eq!(
            line,
            "@s-a post to=@s-b@n-c kind=task re=7 dl=900 via=s-inner01 len=7"
        );
        assert_eq!(body, b"one\ntwo");
        assert!(!line.contains('\n'));
    }

    /// The mirror NEVER follows a symlink out of its root. Without this, an
    /// agent that replaced `inbox.ndjson` with a link to a file the operator can
    /// write would have peers' message text appended to that file.
    #[test]
    fn a_symlinked_mirror_file_is_refused() {
        let dir = scratch("symlink");
        let outside = dir.join("outside.txt");
        std::fs::write(&outside, "untouched\n").expect("write");
        std::fs::create_dir_all(dir.join("s-five")).expect("mkdir");
        std::os::unix::fs::symlink(&outside, dir.join("s-five/inbox.ndjson")).expect("symlink");
        let err = InboxFile::open(&dir, "s-five").expect_err("a symlink must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(
            std::fs::read_to_string(&outside).expect("read"),
            "untouched\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A HARD LINK IS THE OTHER KIND OF LINK, AND THE WALL WAS ONLY A SYMLINK
    /// WALL.** `lstat` reports a hard link as an ordinary file, `O_NOFOLLOW`
    /// binds only a SYMBOLIC final component, and the inode never changes — so
    /// [`Plane::open_tracked`]'s two independent checks both matched. No race is
    /// involved: the agent plants the link at leisure, before or after the
    /// mirror attaches, and every delivered row is appended to the linked-to file
    /// with the operator's uid rather than the agent's.
    ///
    /// All four names, because the finding was reported on one of them and the
    /// hole is the class: `inbox.ndjson` writes a peer's bytes, `sent.ndjson`
    /// writes receipts, `outbox.ndjson` READS a file the agent could not
    /// otherwise reach and posts it to the fabric, and `.cursor.tmp` is the one
    /// open that used to TRUNCATE — which lands inside `open(2)`, before any
    /// check of ours, and needs nothing written through it to destroy the file.
    #[test]
    fn a_hard_linked_mirror_file_is_refused() {
        let dir = scratch("hardlink");
        let outside = scratch("hardlink-outside");
        let victim = outside.join("authorized_keys");
        std::fs::write(&victim, "untouched\n").expect("write");

        // INBOUND. `ln <victim> <root>/.aterm/<sid>/inbox.ndjson` — same
        // filesystem, no symlink, nothing the agent does not already have.
        std::fs::create_dir_all(dir.join("s-hard")).expect("mkdir");
        std::fs::hard_link(&victim, dir.join("s-hard/inbox.ndjson")).expect("ln");
        let err = InboxFile::open(&dir, "s-hard").expect_err("a hard link must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read"),
            "untouched\n",
            "a peer's message text was appended to a file outside the root"
        );

        // OUTBOUND, all three files. `sent.ndjson` is a write; `outbox.ndjson`
        // is a READ that would have relayed the operator's file onto the bus.
        let secret = "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"OPERATORS-PRIVATE-FILE\"}";
        std::fs::write(outside.join("secret.ndjson"), format!("{secret}\n")).expect("write");
        std::fs::create_dir_all(dir.join("s-out")).expect("mkdir");
        std::fs::hard_link(&victim, dir.join("s-out/sent.ndjson")).expect("ln");
        let err = OutboxFile::open(&dir, "s-out").expect_err("a hard link must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        std::fs::remove_file(dir.join("s-out/sent.ndjson")).expect("rm");
        // `OutboxFile::open` creates the outbox before it reaches the receipt
        // file, so the name is there to replace.
        std::fs::remove_file(dir.join("s-out/outbox.ndjson")).expect("rm");
        std::fs::hard_link(
            outside.join("secret.ndjson"),
            dir.join("s-out/outbox.ndjson"),
        )
        .expect("ln");
        let err = OutboxFile::open(&dir, "s-out").expect_err("a hard link must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");

        // THE TRUNCATING OPEN. The cursor's temp is the only one, and a plain
        // `O_CREAT|O_TRUNC` had already emptied the victim by the time any check
        // could refuse the leaf.
        let mut f = OutboxFile::open(&dir, "s-cur").expect("open");
        std::fs::hard_link(&victim, dir.join("s-cur/.cursor.tmp")).expect("ln");
        f.commit(9);
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read"),
            "untouched\n",
            "the cursor's temp truncated a file outside the root"
        );

        // AND THE WINDOW BEHIND IT, WHICH NO TEST HERE CAN RACE. The assertion
        // above passes on `O_TRUNC` too, because the path-side check refuses the
        // link before the open; what `O_TRUNC` leaves is the gap between that
        // check and the `open`, where a truncate lands before any check of ours
        // can run. That is a source property, so it is pinned as one — the house
        // pattern this file already uses for a claim a run cannot exercise.
        let me = include_str!("mirror.rs");
        let durable = me
            .split_once("fn write_durable")
            .and_then(|(_, tail)| tail.split_once("\n    /// "))
            .map_or("", |(body, _)| body);
        assert!(!durable.is_empty(), "write_durable moved; repoint this pin");
        assert!(
            durable.contains("create_new(true)") && !durable.contains("truncate(true)"),
            "the cursor's temp must be O_EXCL, never O_TRUNC:\n{durable}"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// **THE WALL HOLDS AFTER THE FIRST CHECK.** The mirror validated
    /// `<root>/.aterm/<sid>` once, at open, and every later append re-resolved
    /// the whole path with only the LEAF guarded: `symlink_metadata` follows the
    /// parent components before it stats the leaf, and `O_NOFOLLOW` binds the
    /// final component and nothing else. So the confined agent — who owns the
    /// root, by construction — could move its own session directory aside, point
    /// the name at somewhere outside the sandbox, and have every later peer
    /// message appended THERE, under the operator's authority.
    ///
    /// The target file already exists here, and that is the scenario: an append
    /// with no `create` needs one, and appending attacker-chosen JSON to a file
    /// the operator can write is the whole of the escape.
    #[test]
    fn a_swapped_session_directory_stops_the_next_append() {
        let dir = scratch("swapdir");
        let outside = scratch("swapdir-outside");
        std::fs::write(outside.join("inbox.ndjson"), "untouched\n").expect("write");

        let mut f = InboxFile::open(&dir, "s-swap").expect("open");
        f.append(&row(1, 1), "before")
            .expect("the first append lands");

        // `mv <sid> keep && ln -s /elsewhere <sid>` — everything the agent needs,
        // and nothing it does not already have.
        std::fs::rename(dir.join("s-swap"), dir.join("keep")).expect("mv");
        std::os::unix::fs::symlink(&outside, dir.join("s-swap")).expect("ln -s");

        let err = f
            .append(&row(2, 2), "after")
            .expect_err("an append through a swapped directory must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(
            std::fs::read_to_string(outside.join("inbox.ndjson")).expect("read"),
            "untouched\n",
            "a peer's message text was written outside the sandbox root"
        );
        // And the row is NOT recorded as written, so it is appended once the
        // plane is whole again rather than lost to a refusal.
        assert!(!f.holds(2));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The same wall on the OUTBOUND side, where the escape is three: `.cursor`
    /// is created and renamed outside the root, `sent.ndjson` is appended to
    /// outside it, and the lines that get posted to the fabric are READ from
    /// outside it — a file the agent could not otherwise reach, relayed onto the
    /// bus under the mirror's authority.
    #[test]
    fn a_swapped_session_directory_stops_the_outbound_side_too() {
        let dir = scratch("swapout");
        let outside = scratch("swapout-outside");
        let secret = "{\"to\":\"@s-b\",\"kind\":\"note\",\"text\":\"OPERATORS-PRIVATE-FILE\"}";
        std::fs::write(outside.join("outbox.ndjson"), format!("{secret}\n")).expect("write");
        std::fs::write(outside.join("sent.ndjson"), "untouched\n").expect("write");

        let mut f = OutboxFile::open(&dir, "s-swap").expect("open");
        std::fs::rename(dir.join("s-swap"), dir.join("keep")).expect("mv");
        std::os::unix::fs::symlink(&outside, dir.join("s-swap")).expect("ln -s");

        assert!(
            f.take().is_empty(),
            "the mirror read an outbox from outside its root and would have posted it"
        );
        let err = f
            .note("{\"at\":1}")
            .expect_err("a receipt through a swapped directory must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        f.commit(9);
        assert_eq!(
            std::fs::read_to_string(outside.join("sent.ndjson")).expect("read"),
            "untouched\n"
        );
        assert!(
            !outside.join(".cursor").exists() && !outside.join(".cursor.tmp").exists(),
            "the cursor was written outside the sandbox root"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// A ROTATION IS A PATH OPERATION TOO. Crossing [`ROTATE_BYTES`] renames the
    /// file and `touch`es a new one, and `touch` CREATES — so through a swapped
    /// directory it is a file created outside the sandbox and then appended to,
    /// which needs no pre-existing target at all.
    #[test]
    fn a_swapped_session_directory_stops_a_rotation() {
        let dir = scratch("swaprot");
        let outside = scratch("swaprot-outside");
        let mut f = InboxFile::open(&dir, "s-swap").expect("open");
        // The operator's own file, big enough that the rotation branch fires on
        // it: the rename moves it aside and `touch` creates a replacement, so
        // this path needs no pre-existing target to write to — it makes one.
        let victim = outside.join("inbox.ndjson");
        std::fs::write(
            &victim,
            vec![b'\n'; usize::try_from(ROTATE_BYTES).expect("fits")],
        )
        .expect("a file the operator can write");
        std::fs::rename(dir.join("s-swap"), dir.join("keep")).expect("mv");
        std::os::unix::fs::symlink(&outside, dir.join("s-swap")).expect("ln -s");

        let err = f
            .append(&row(3, 3), "after")
            .expect_err("a rotation through a swapped directory must be refused");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");
        assert_eq!(
            std::fs::metadata(&victim).expect("the victim file").len(),
            ROTATE_BYTES,
            "the operator's file was rotated away from under it"
        );
        assert!(
            !outside.join("inbox.ndjson.1").exists(),
            "the rotation renamed a file outside the sandbox root"
        );
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// **AN EVICTION THE AGENT NEVER SAW IS REPORTED IN THE AGENT'S OWN FILE.**
    /// `fabric.rs` promises `dropped=` makes an eviction "REPORTED, never
    /// silent"; the mirror read the rows of every listing and threw the header
    /// away, so on the one plane whose agent has no other channel the promise
    /// was kept to nobody.
    ///
    /// A notice is deliberately NOT a row: it carries no `off`, so a reader
    /// keyed on the row vocabulary skips it and the `off=` idempotency window
    /// never sees it.
    #[test]
    fn an_eviction_is_reported_on_the_file_plane_too() {
        let dir = scratch("dropped");
        let mut f = InboxFile::open(&dir, "s-drop").expect("open");
        let header = "OK 2 hold=0 holder=- seen=0 bus_head=90340 dropped=3 pending=1";
        assert!(
            dropped_notice(
                "OK 0 hold=0 holder=- seen=0 bus_head=0 dropped=0 pending=0",
                0
            )
            .is_none(),
            "nothing dropped is nothing said"
        );
        assert!(
            dropped_notice("OK 0 hold=0", 0).is_none(),
            "a header without the field must not invent a notice"
        );
        let (total, line) = dropped_notice(header, 0).expect("a notice for the first three");
        assert_eq!(total, 3);
        f.notice(&line).expect("write the notice");
        assert!(
            dropped_notice(header, total).is_none(),
            "the same count must not be reported twice"
        );
        let (total, line) = dropped_notice(
            "OK 2 hold=0 holder=- seen=0 bus_head=90340 dropped=5 pending=1",
            total,
        )
        .expect("a notice for the next two");
        assert_eq!(total, 5);
        assert!(line.contains("\"new\":2"), "{line}");

        let body = std::fs::read_to_string(dir.join("s-drop/inbox.ndjson")).expect("read");
        assert_eq!(body.lines().count(), 1, "one line per notice: {body}");
        let obj = json_object(body.trim()).expect("valid JSON");
        assert_eq!(obj.get("notice").map(String::as_str), Some("dropped"));
        assert_eq!(obj.get("dropped").map(String::as_str), Some("3"));
        assert!(!obj.contains_key("off"), "a notice is not a row: {body}");
        assert!(!f.holds(3), "a notice must not enter the dedup window");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The JSON reader is TOTAL over hostile bytes: no panic, no runaway, and a
    /// half-understood object is `None` rather than a partly-filled map.
    #[test]
    fn the_json_reader_is_total() {
        for src in [
            "",
            "{",
            "}",
            "{}",
            "[]",
            "{\"a\"}",
            "{\"a\":}",
            "{\"a\":1,}",
            "{\"a\":1} trailing",
            "{\"a\":\"\\u\"}",
            "{\"a\":\"\\q\"}",
            "{\"a\":\"\\ud800\"}",
            "{\"a\":\"\\udc00\"}",
            "{\"a\":{\"b\":{\"c\":[1,2,{\"d\":[]}]}},\"e\":2}",
            "{\"a\":--1}",
            "{\"a\":\"\u{1f600}\"}",
        ] {
            let _ = json_object(src);
        }
        assert_eq!(json_object("{}"), Some(BTreeMap::new()));
        let obj = json_object("{\"a\":{\"b\":[1,2]},\"n\":-1.5e3,\"s\":\"\\ud83d\\ude00\"}")
            .expect("nested containers are consumed and discarded");
        assert_eq!(obj.get("a").map(String::as_str), Some(""));
        assert_eq!(obj.get("n").map(String::as_str), Some("-1.5e3"));
        assert_eq!(obj.get("s").map(String::as_str), Some("\u{1f600}"));
        // Trailing junk is a refusal, not a prefix parse: two objects on one
        // line would otherwise post the first and silently drop the second.
        assert_eq!(json_object("{\"a\":1}{\"b\":2}"), None);
    }

    /// The `msg` row parser reads what `render_row` writes, in the order it
    /// writes it, and pct-decodes the text exactly once.
    #[test]
    fn a_msg_row_parses_field_for_field() {
        let line = "msg 41 off=90312 t=1756389000123 from=s-7c1e@n-b2f0 kind=note \
                    trust=relayed re=90300 re-id=8 dl=240000 late=1 demoted=task \
                    via=s-inner01 len=34 more=1 text=which%20branch%3F";
        let row = parse_msg_row(line).expect("a msg row");
        assert_eq!(row.id, 41);
        assert_eq!(row.off, 90_312);
        assert_eq!(row.from, "s-7c1e@n-b2f0");
        assert_eq!(row.kind, "note");
        assert_eq!(row.trust, "relayed");
        assert_eq!(row.re, Some(90_300));
        assert_eq!(row.re_id, Some(8));
        assert_eq!(row.dl, Some(240_000));
        assert!(row.late && row.more);
        assert_eq!(row.demoted.as_deref(), Some("task"));
        assert_eq!(row.via.as_deref(), Some("s-inner01"));
        assert_eq!(row.text, "which branch?");
        assert!(parse_msg_row("post 7 to=x kind=ask off=-").is_none());
        assert!(parse_msg_row("OK 2 hold=0").is_none());
    }

    /// The argv parser takes §11.2's `--mirror <root>` spelling and a bare
    /// positional root, and refuses a session id that is not one.
    #[test]
    fn the_argv_parser_takes_the_designs_spelling() {
        let args = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let a = parse_args(&args(&["--mirror", "/w", "--sock", "/s"])).expect("flag form");
        let b = parse_args(&args(&["/w", "--sock", "/s"])).expect("positional form");
        assert_eq!(a.root, b.root);
        assert_eq!(a.interval, POLL_DEFAULT);
        assert!(parse_args(&args(&["/w"])).is_err(), "--sock is required");
        assert!(
            parse_args(&args(&["--sock", "/s"])).is_err(),
            "a root is required"
        );
        assert!(parse_args(&args(&["/w", "--sock", "/s", "--session", "nope"])).is_err());
        assert!(parse_args(&args(&["/w", "--sock", "/s", "--mirror"])).is_err());
        let clamped = parse_args(&args(&["/w", "--sock", "/s", "--interval", "0"])).expect("ok");
        assert_eq!(clamped.interval, POLL_MIN);
    }
}
