//! `asb` — a thin CLI over the astream broker, so any language or shell can put
//! bytes on the bus, tail them, read retained state, and drain an inbox. Every
//! verb is a direct pass-through to the claim-tested [`astream_broker`] library,
//! over a Unix socket or (with `--tcp`) TCP:
//!
//! ```text
//! asb serve  <endpoint> [<log>] [--tcp] [--secret-file PATH] [--unix SOCKET] [--durability T]
//!                                                           run a broker; prints `listening <bound>`
//! asb pub    <endpoint> <subject> [--id N] [--seq M]        publish stdin's bytes; prints `<offset> new|dup`
//! asb sub    <endpoint> <filter> [--from N | --group G]     tail as `<offset> <nbytes>\n<bytes>\n`
//! asb last   <endpoint> <filter> [--after S] [--max N]      last record per subject, then `MARK next= head=`
//! asb fetch  <endpoint> <filter> [--from N] [--max M]       a bounded, non-tailing read, then `MARK`
//! asb drain  <endpoint> <group> <filter> [--max N] [--idle MS] [--peek]
//! asb ack    <endpoint> <group> <offset> <subject> handled|refused|deferred
//! asb commit <endpoint> <group> <upto>                      durably commit a consumer group
//! asb mint   <grant> --secret-env NAME | --secret-file PATH mint a capability line
//! asb repair <log>                                          truncate at the first corrupt record
//! asb stats  <log>                                          inspect log offsets, retention, subjects, and bindings
//! ```
//!
//! `stats` opens a log no broker serves and reads it. A log a running broker serves is
//! locked to it, so `stats` reads that broker's health sidecar, `<log>.health`, instead
//! — `serve` keeps one (every `--health-ms`, default 2 s, and at once when the log is
//! poisoned) — and prints it only while the snapshot can be current: `writer=serving`
//! when the broker that wrote it still holds it, `writer=unknown` where this host keeps
//! no advisory locks. A snapshot a gone broker left is not printed (exit 1).
//!
//! `commit`'s printed `<offset>` is the COMMIT RECORD's own log offset, NOT
//! `<upto>` and not the group's committed offset: the two live in different
//! spaces, so never resume a consumer from it — resume with `sub --group G`.
//!
//! `commit`'s `<upto>` and `ack`'s `<offset>` must name a record the broker HAS: both
//! are checked against its visible head first, and an operand at or past it is a usage
//! error (exit 2, nothing durable done). A consumer group's committed offset is
//! MONOTONE by construction — no asb verb and no broker request lowers it — so an
//! over-large one would durably skip G past every record it had not read, with no
//! recovery but a new group name. That check is why `commit` also requires `<group>`
//! to be subject-shaped: a guarded broker already does (a group name is a subject the
//! ring must grant), and the head probe reads under that same name so it is authorized
//! by the same grant the commit is.
//!
//! `<endpoint>` is a socket path, or a `host:port` under `--tcp` (serve accepts
//! `host:0` and prints the OS-chosen port). `sub`'s framing — a header line then
//! exactly `<nbytes>` raw body bytes then a newline — is byte-exact and NUL-safe,
//! the shape a lash/bridge parses to move a terminal's `/out` and `/in`, locally
//! or across a machine boundary.
//!
//! `last`, `fetch` and `drain` print the SUBJECT in the header —
//! `<offset> <nbytes> <subject>\n<body>\n` — where `sub` does not. The reason is
//! the query: those three are wildcard reads over a whole subtree, and a drain
//! that had to guess which subject a record came from could not route it. The
//! COUNT comes BEFORE the subject because the subject is the one field whose
//! bytes a publisher chooses: the wire rejects a control byte in a subject but
//! NOT a space, so `<offset> <subject> <nbytes>` could be spoofed by publishing
//! to `/f/F/in/evil 999` and desynchronizing every consumer that splits the
//! header on spaces. With the two numeric fields first the parse is exact — read
//! `<offset>`, read `<nbytes>`, and the SUBJECT IS THE REST OF THE LINE (never
//! empty, always `/`-leading, and newline-free by the wire's own grammar) — so no
//! subject can fake a header. `last` and `fetch` then print
//! `MARK next=<n> head=<h>`. `next` means a DIFFERENT thing in each: for `fetch` it
//! is the page cursor — the offset after the last record SCANNED — so `--from
//! <next>` reaches the rest, and `next` below `head` means there is more. For `last`
//! it is the head itself, the offset to `sub --from` to tail on from this answer, and
//! NOT a page cursor. `last` pages by SUBJECT instead: `--max` bounds the ROW COUNT,
//! a page shorter than `--max` — an empty one included — ends the subject walk, and a
//! full page continues with `--after <the last subject printed>`.
//!
//! WHAT `last`'s MARK IS, AND IS NOT. For `fetch`, `head` is the subscriber-visible
//! head that one request was answered at. For `last` it is not quite that, and the
//! difference is worth two paragraphs. ONE `Last` request is a snapshot: the broker
//! pins the visible head on its first scan round and reads every later round against
//! that pinned value. But asb follows the broker's resume cursor past its internal row
//! and index-scan bounds (below), so one `asb last` can be SEVERAL requests, each
//! pinning its own, LATER head. The printed `next`/`head` is the FIRST request's — the
//! lowest of them — because that is the only offset a paired `sub --from <next>` can
//! start at with no gap: from any later head, a record that superseded a value an
//! earlier request already printed would never be delivered. So the printed answer is
//! a UNION of per-request snapshots reported under the first one's head, not one
//! snapshot read at that head, and a row from a later request may carry an offset AT
//! OR ABOVE the printed `head`. Fold newest-wins when you tail from `next`, exactly as
//! a client paging `Last` by hand must.
//!
//! AND A SHORT `last` PAGE HAS TWO CAUSES, NOT ONE. Following the resume cursor takes
//! the broker's row and index-scan limits out of the answer: neither of those can
//! truncate it silently. The head pin has a cost that stays, deliberately.
//! The broker decides each subject from its LATEST offset and skips it entirely when
//! that offset is not below the pinned head, so a subject the walk has not reached yet
//! whose newest record lands at or above that head is OMITTED from the page rather
//! than returned at its older value — fail-closed, the direction the Replicated tier
//! already takes above the quorum watermark. At the broker that costs nothing: the
//! answer's `next` IS the pinned head, so the record that displaced the omitted
//! subject arrives on the reader's paired `sub --from <next>`. `asb last` ALONE does
//! not make that read — it prints rows and a MARK and exits — so on the shell face the
//! omission is real and invisible: the row count is one lower, `--max` was not
//! reached, and nothing says why. THE WHOLE ANSWER IS `--max` PLUS `sub --from
//! <next>`, never `--max` alone. A fleet roster read out of `asb last` on its own can
//! be one live node short.
//!
//! Durable resume for a shell consumer: `asb sub <ep> <filter> --group G` resumes
//! from the group's durable committed offset (`upto + 1`, or 0 if never
//! committed) and `asb commit <ep> G <upto>` advances it after the record at
//! `upto` has been acted on — so a restarted consumer never re-processes history.
//! `asb drain` is the batched form of that pair, and `asb ack` is the atomic
//! read-process-write form: the answer record and the cursor advance in ONE
//! durable append, deduped by the input offset, so a retried ack appends nothing.
//!
//! `ack` publishes under the producer sequence `2^63 | <input offset>`, and
//! `pub` refuses a `--seq` at or above `2^63`: the TOP HALF of the producer
//! sequence space is RESERVED for acks. Under one bound grant `pub` and `ack`
//! share a single producer id (the grant permits no other), and the broker dedups
//! on `(producer_id, producer_seq)` — so a `pub --seq-file` counter (1, 2, 3, …)
//! and an early input offset would otherwise be the SAME dedup key, and whichever
//! arrived second would be swallowed: nothing appended, no cursor moved, `dup`
//! printed, exit 0. Reserving a half makes the two spaces disjoint by
//! construction, and a retried ack still dedups because its sequence is a
//! function of the input offset alone.
//!
//! The reservation is NOT asb's alone: `ACK_SEQ_BASE` is exported from the library
//! and `astream_broker::ack` derives the same `ACK_SEQ_BASE | offset` key, so an
//! ack retried through the OTHER face — a bridge that shells out here on one path
//! and links the crate on another — is recognised as the retry it is and dedups.
//! That agreement is pinned by `broker.ack-key-is-one-wire-contract`.
//!
//! Plain `--tcp` is plaintext (trusted network only). For the XChaCha20-Poly1305-
//! sealed transport (asb built `--features aead`) supply the 32-byte pre-shared
//! key as 64 hex chars through `--key-env NAME` (an environment variable) or
//! `--key-file PATH` — never on argv: a bare `--key HEX` is refused, because argv
//! is visible to every local user via `ps` for the process's whole lifetime.
//!
//! A capability is presented the same way and for the same reason: `--cap-file
//! <path>` (repeatable) reads `<grant> <tag-hex>` lines — exactly what `asb mint`
//! prints — and attaches each of them BEFORE the verb runs. The line is split at its
//! LAST whitespace, not its first, because the wire admits a SPACE inside a filter
//! and so a grant may hold one: the tag is the fixed-width whitespace-free tail, so
//! `ro:/f/F/pub a/> <tag>` reads back as the grant `mint` sealed rather than as a
//! parse error that takes the whole ring down with it. Both the line `mint` prints
//! and the reader are `astream_cap::capfile`'s, so the two cannot disagree.
//! `--cap-filter F --cap-tag HEX` on argv is REFUSED: a capability tag is a secret,
//! and argv is world-readable. The attach is a PROOF OF POSSESSION over the broker's
//! per-connection nonce, so the tag itself never crosses the wire; computing that
//! proof needs the same vetted MAC the broker uses, so `--cap-file` and `mint`
//! need `asb` built with `--features cap`. An unguarded broker acknowledges the
//! attach and enforces nothing.
//!
//! `asb mint` REQUIRES an explicit mode (`ro:` / `rw,p=<principal>:`), and it
//! gates on the PARSED grant, never on its spelling: the read-write grant that
//! names no principal is the UNBOUND god cap — it may publish under any producer
//! id — whether it is written `/f/F/>` or `rw:/f/F/>`, which `Grant::parse` maps
//! to exactly the same authority. Minting either takes `--legacy-unbound`, so a
//! careless mint cannot silently hand out fleet root through the other spelling.
//! When exactly one attached WRITABLE grant names a principal, `pub` and `ack`
//! derive their producer id from it (`astream_cap::producer_id_of`), which is the
//! only id such a grant may publish under. A read-only grant binds nothing (the
//! broker binds nothing for it either), so a `ro,p=<principal>` grant on the ring
//! never chooses the id anything publishes under.
//!
//! A GUARDED BROKER FROM THE CLI. `asb serve --secret-env NAME | --secret-file
//! PATH` opens `Broker::open_guarded` under that mint secret (asb built with
//! `--features cap`): every attach must carry a capability `asb mint` sealed under
//! the same secret, and every request is authorized against the grants attached.
//! Without a secret the broker checks nothing on attach, and a Unix socket's only
//! boundary is the DIRECTORY it sits in. The secret is held to the same rules
//! `mint` reads it under — the same bytes (a file exactly, an environment variable trimmed), at least [`capfile::SECRET_MIN`] bytes, and a
//! `--secret-file` readable by its owner alone (mode 0600 or tighter), which
//! `serve` also demands of its `--key-file`: a daemon that runs for weeks on a
//! group-readable key has been handing it out the whole time. Both are the one
//! secret-file rule of `astream_cap::capfile` (`read_secret_file`, and
//! `read_private_file` for the hex key), the file this binary already compiles in
//! for `--cap-file`: each reads the mode off the descriptor its bytes come from,
//! so the file whose mode passed is the file whose key is used.
//!
//! A TCP LISTENER BINDS LOOPBACK UNLESS TOLD OTHERWISE. `serve` refuses an
//! endpoint that resolves to anything but loopback — `0.0.0.0`, `[::]`, a LAN
//! address — without `--allow-remote`, because a pre-shared key is one secret
//! every host holds (a transport boundary, not a per-host identity) and plain
//! `--tcp` has no boundary at all. `--unix SOCKET` serves a Unix socket AS WELL,
//! from the same broker, log and guard, so the host that serves the network keeps
//! its own clients off the TCP port: the sealed wire admits a bounded number of
//! connections into its handshake at once, and a peer that can reach the port can
//! hold them without the key. A refused bind leaves no empty log behind.
//!
//! Every flag is strict: an unknown `--flag`, a non-numeric `--id/--seq/--from/
//! --max/--idle`, and a flag that does not apply to the verb are usage errors
//! (exit 2) rather than silent defaults, so a mistyped flag can never quietly
//! downgrade a transport or defeat an idempotent retry. Because that strictness
//! catches every dash-leading token, `--` ends flag parsing: each token after it
//! is a positional (the escape for a log path, subject, filter or group that
//! begins with `-`). Argv that is not valid UTF-8 is a usage error too, never a
//! panic.
//!
//! EXIT STATUS, AND ONLY THESE THREE: `0` the verb did what it says; `1` a runtime
//! failure (connect, broker error, a log it cannot open, an I/O error on stdout); `2`
//! a usage error, with nothing written and nothing done. There is no `101`: a write
//! error on stdout or stderr is reported through that contract rather than through a
//! panic, because a supervisor that reads 2 as "bad config, do not retry" and 1 as
//! "transient, retry" can classify a Rust backtrace as neither — and for `pub`, whose
//! default sequence is a fresh timestamp, the retry it guesses at puts the record on
//! the bus a second time. When stdout is gone but stderr is not, a verb that already
//! made a DURABLE change (`pub`, `ack`, `commit`, a `repair` that truncated) repeats
//! what it did on stderr, so the offset is not lost with the pipe. When stderr is gone
//! too, the diagnostic is dropped and the status is the whole message.
#![forbid(unsafe_code)]

// A contracted fn is written once, through `contract_fn!`: under a Trust compiler
// (`cfg(clean_islands)`, build.rs) it carries its `ensures` clause, which no stock rustc can
// parse, and every other compiler gets the plain fn.
#[macro_use]
#[path = "../../../astream-wire/contract_fn.rs"]
mod contract_fn;

#[cfg(clean_islands)]
#[path = "islands/asb_island.rs"]
mod asb_island;
#[cfg(clean_islands)]
pub use asb_island::asb_drain_count_advance;

#[cfg(any(test, not(clean_islands)))]
#[path = "islands/asb_twins.rs"]
mod twins;
#[cfg(not(clean_islands))]
pub use twins::asb_drain_count_advance;

contract_fn! {
    /// Verified check that an `asb` exit code is within the documented `{0, 1, 2}` contract
    /// (`code <= 2`), linked via `clean-kernel-defeq` to `asb_exit_code_valid_isl`.
    #[must_use]
    pub const fn asb_exit_code_valid(code: u8) -> bool
    clauses [ensures result == (code <= 2)]
    {
        code <= 2
    }
}

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::process::ExitCode;
use std::time::Duration;

use astream_broker::{
    AnyClient, AnySubscription, Broker, BrokerHandle, BrokerLog, Closer, Durability, Event, Gap,
    Record, Transport, Walk, MAX_RECORD_PAYLOAD,
};

// The ONE capability-file format and reader, `astream_cap::capfile`: what `mint`
// prints and what `--cap-file` reads, by one set of rules; and the ONE secret-file
// rule, which `--secret-file` and `--key-file` are read by.
//
// A default build has no `astream-cap` dependency — capabilities are the opt-in
// `cap` feature — yet it still reads `--cap-file` (a malformed line is
// exit 2 naming file:line in every build; only the attach needs the MAC), and
// still holds a `--key-file` to the owner-only rule. So it compiles that module's
// source file (std and astream-sys only) in by path, rather than keeping a second
// reader (or a second secret-file rule, which this binary did) that could drift from
// the first.
#[cfg(feature = "cap")]
use astream_cap::capfile;
#[cfg(not(feature = "cap"))]
#[allow(dead_code)] // `format_line` is `mint`'s, and `mint` needs `cap`
#[path = "../../../astream-cap/src/capfile.rs"]
mod capfile;

const USAGE: &str = "usage: asb serve  <endpoint> [log] [--tcp] [--key-env NAME | --key-file PATH] [--handshake]
                  [--secret-env NAME | --secret-file PATH] [--revocation-file PATH] [--unix SOCKET] [--allow-remote]
                  [--durability strict|ordered|relaxed] [--horizon-ms N] [--health-ms N]
                  [--retain-file PATH] [--retention-interval-ms N]
                  [--max-retained-records N] [--max-retained-bytes N]
       asb pub    <endpoint> <subject> [--id N] [--seq M | --seq-file PATH] <transport> <cap>
       asb sub    <endpoint> <filter> [--from N | --group G] <transport> <cap>
       asb last   <endpoint> <filter> [--after SUBJECT] [--max N] <transport> <cap>
       asb fetch  <endpoint> <filter> [--from N] [--max N] <transport> <cap>
       asb drain  <endpoint> <group> <filter> [--max N] [--idle MS] [--peek] <transport> <cap>
       asb ack    <endpoint> <group> <offset> <subject> handled|refused|deferred [--id N] <transport> <cap>
       asb commit <endpoint> <group> <upto> <transport> <cap>
       asb mint   <grant> --secret-env NAME | --secret-file PATH [--legacy-unbound]
       asb attenuate <child-grant> --cap-file PATH [--secret-env NAME | --secret-file PATH]
       asb repair <log>
       asb stats  <log>
  <transport>:                      [--tcp] [--key-env NAME | --key-file PATH] [--handshake]
  <cap>:                            [--cap-file PATH]...   (repeatable; `<grant> <tag-hex>` lines, as `asb mint` prints;
                                    split at the LAST whitespace, so a grant whose filter holds a space reads back)
  --handshake:                      with a key, add an ephemeral X25519 key agreement in front of the sealed
                                    wire (forward secrecy: a later PSK compromise cannot decrypt a recorded
                                    session); needs asb built with --features handshake
  repair:                           truncate a log at its first CORRUPT record (which `serve` refuses to open,
                                    because the bytes from there may be acked data) and report what was dropped;
                                    keyless, so a log sealed at rest is refused, never cut (repair it with its key)
  stats:                            print operational log counters (`base`, `head`, `records`, `bytes`, `subjects`,
                                    `producers`, `wills`, `groups`, `replica`) for `<log>`; for a log a running
                                    broker serves, read them from its health sidecar `<log>.health` (which
                                    `serve` keeps), with the broker's heads, connections, parked tails, served
                                    tier, flush counts, `writer=` (serving, or unknown where this host cannot
                                    tell), `pid=`, `at_ms=`, and `poisoned=` last (`no`, or `yes <reason>`);
                                    exit 1 when no current snapshot exists
  --health-ms N:                    serve: rewrite the health sidecar `<log>.health` every N ms (default 2000), and
                                    at once when the log is poisoned
  --revocation-file PATH:           load revoked 64-hex Ed25519 keys or capability principals at `serve` startup
                                    and on Apple targets reload on `SIGHUP` (fail-closed on malformed edits)
  --key-env NAME / --key-file PATH: 64 hex chars (32-byte PSK) for the XChaCha20-Poly1305-sealed TCP transport
                                    (needs asb built with --features aead); a bare --key HEX on argv is refused
  --cap-file PATH:                  present minted capabilities to a guarded broker, as a proof of possession
                                    over its nonce (needs asb built with --features cap); --cap-filter/--cap-tag
                                    on argv are refused -- a tag is a secret and argv is world-readable
  --secret-env / --secret-file:     the MINT secret, never on argv (a bare --secret is refused): `asb mint` seals
                                    with it, and `asb serve` given one opens a GUARDED broker (needs --features
                                    cap) that refuses an attach whose capability was not minted under it. At
                                    least 32 bytes; a --secret-file is read BYTE FOR BYTE (a trailing newline is
                                    part of the secret) and --secret-env is trimmed; a --secret-file (and
                                    serve's --key-file) must be readable by its owner alone (0600)
  --unix SOCKET:                    serve with a TCP listener: ALSO serve this Unix socket, from the same broker,
                                    log and guard, so the host's own clients never queue behind the TCP port
  --allow-remote:                   serve with a TCP listener: bind an endpoint that is not loopback (0.0.0.0,
                                    [::], a LAN address). Without it only 127.0.0.1 / ::1 are bound
  --durability T:                   serve: what an ack promises -- strict (the default: on stable media), ordered
                                    (ordered on the device, F_BARRIERFSYNC; a power loss can take the newest
                                    batches and moves the log's generation; served strict off Apple or off the
                                    boot volume, never weaker), relaxed (the page cache). asb escalates nothing
  --horizon-ms N:                   serve --durability ordered: flush in full once N ms have passed since the first
                                    ack no full flush covers, after the batch in hand -- so that ack reaches stable
                                    media at most N ms + one batch + one full flush after it (default 1000; 0: a
                                    full flush after every batch)
  --retain-file PATH:               serve: retain the log by the rules in PATH (REQUIREMENTS R4), one per line,
                                    `#` comments: `prefix|hold|default [age=<n>ms|s|m|h|d] [bytes=<n>[KiB|MiB|GiB]]
                                    [last] [read=<group template>] <filter>` (no filter on `default`). Exactly one
                                    prefix rule sets the cut; hold rules carry what they keep below it in <log>.held.
                                    Needs asb built with the retention feature (the default)
  --retention-interval-ms N:        serve: run the retention tick at least every N ms (default 60000; a batch past
                                    what the rules' bytes or a cap allow asks for one at once)
  --max-retained-records N / --max-retained-bytes N:
                                    serve: compact, carrying state, when the log holds more than N records / bytes
  --legacy-unbound:                 mint the read-write UNBOUND god cap -- `/f/F/>` or `rw:/f/F/>`, one authority in
                                    two spellings -- which `mint` otherwise refuses; a grant needs an explicit `ro:`
                                    mode, or `rw,p=<principal>:` to write as exactly that principal
  --seq M:                          an explicit producer sequence, so a retry of the same (id, seq) is deduped
                                    (needs a stable --id or one bound grant, like --seq-file)
  --seq-file PATH:                  a write-ahead persisted producer sequence (needs a stable --id or one bound
                                    grant): the file is advanced and fsynced BEFORE the publish, so a crash
                                    between the two BURNS that sequence number -- never a duplicate, but the
                                    record is simply never published. A file that does not hold an unsigned
                                    integer -- INCLUDING an empty or whitespace-only one -- is an error, never a
                                    restart at 1 (a restart would have the broker dedup live records away).
                                    Concurrent invocations on one file are SERIALIZED by an exclusive advisory
                                    lock on a sibling <PATH>.lock, so two publishes a millisecond apart cannot
                                    read the same number and have the broker dedup one of them away
  --idle MS:                        drain's idle window, a POSITIVE number of milliseconds (0 is refused here
                                    rather than by the OS after the group subscription is already registered)
  --group G:                        resume from the group's durable committed offset (see `asb commit`)
  --peek:                           drain without committing: the same records are delivered again next time
  --:                               ends flag parsing; every later token is a positional (for one beginning with `-`)
  sub/drain say on STDERR what retention gave up below the read, before the records after it:
  `asb: sub|drain: GAP from=<F> to=<T> records=<N|unknown>` (every record of the filter in F..T not delivered is gone)
  last/fetch/drain print `<offset> <nbytes> <subject>` headers, the SUBJECT running to the end of the line (a
  subject may contain a space, never a newline), then the body's raw bytes and ONE newline; sub omits the
  subject and prints `<offset> <nbytes>`; last/fetch then MARK -- fetch's next is the page cursor (resume with
  --from), last's next is the head to sub --from; last pages by SUBJECT (--max bounds the ROWS, a full page
  resumes with --after <the last subject printed>). last's MARK is the FIRST request's -- asb may make several --
  so a printed row can carry an offset at or above the printed head, and a subject superseded past a request's
  pinned head is omitted from its page: the WHOLE answer is --max PLUS a paired `sub --from <next>`
  ack publishes under producer sequence 2^63|<input offset>: the top half of the sequence space is RESERVED for
  acks, so --seq (and --seq-file) must stay below 2^63 and can never collide with an ack under the same id
  commit prints the COMMIT RECORD's own log offset, not <upto> and not the group's committed offset
  commit's <upto> and ack's <offset> must name a record the broker HAS: at or past its visible head is a usage
  error, because a group's committed offset is MONOTONE -- nothing lowers it again, so an over-large one
  durably skips every record the group has not read and the only recovery is a new group name";

/// How many records a `last`/`fetch` page and a `drain` batch take by default.
const DEFAULT_PAGE: u32 = 256;
/// How long `drain` waits with nothing arriving before it stops, by default.
const DEFAULT_IDLE_MS: u64 = 1000;

/// The producer sequence `asb ack` ORs the input offset into: the TOP HALF of the
/// sequence space is reserved for acks and `pub` is held below it (see the module
/// doc). Taken from the library so both faces derive an ack's key from ONE constant.
use astream_broker::ACK_SEQ_BASE;

enum AsbErr {
    Exit(u8),
    Io(io::Error),
}

impl From<u8> for AsbErr {
    fn from(code: u8) -> Self {
        Self::Exit(code)
    }
}

impl From<io::Error> for AsbErr {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

fn fail_exit(code: u8, msg: &str) -> u8 {
    warn(&format!("asb: {msg}"));
    code
}

/// Four-byte magic prefix (`"ASBH"`) for a 24-byte `asb` frame header wire descriptor.
pub const ASB_FRAME_HEADER_WIRE_MAGIC: [u8; 4] = *b"ASBH";

/// Wire format version for `asb` frame header wire descriptors.
pub const ASB_FRAME_HEADER_WIRE_VERSION: u8 = 1;

/// Encode a 24-byte `asb` frame header wire descriptor:
/// `[0..4] = b"ASBH"`, `[4] = version (1)`, `[5] = verb_tag` (`0` = Sub, `1` = Last,
/// `2` = Fetch, `3` = Drain, `4` = Mark), `[6..8] = subject_len (BE)`,
/// `[8..16] = offset_or_next (BE)`, `[16..24] = len_or_head (BE)`.
#[must_use]
pub const fn asb_encode_frame_header_wire(
    verb_tag: u8,
    subject_len: u16,
    offset_or_next: u64,
    len_or_head: u64,
) -> Option<[u8; 24]> {
    let out = [
        ASB_FRAME_HEADER_WIRE_MAGIC[0],
        ASB_FRAME_HEADER_WIRE_MAGIC[1],
        ASB_FRAME_HEADER_WIRE_MAGIC[2],
        ASB_FRAME_HEADER_WIRE_MAGIC[3],
        ASB_FRAME_HEADER_WIRE_VERSION,
        verb_tag,
        ((subject_len >> 8) & 0xFF) as u8,
        (subject_len & 0xFF) as u8,
        ((offset_or_next >> 56) & 0xFF) as u8,
        ((offset_or_next >> 48) & 0xFF) as u8,
        ((offset_or_next >> 40) & 0xFF) as u8,
        ((offset_or_next >> 32) & 0xFF) as u8,
        ((offset_or_next >> 24) & 0xFF) as u8,
        ((offset_or_next >> 16) & 0xFF) as u8,
        ((offset_or_next >> 8) & 0xFF) as u8,
        (offset_or_next & 0xFF) as u8,
        ((len_or_head >> 56) & 0xFF) as u8,
        ((len_or_head >> 48) & 0xFF) as u8,
        ((len_or_head >> 40) & 0xFF) as u8,
        ((len_or_head >> 32) & 0xFF) as u8,
        ((len_or_head >> 24) & 0xFF) as u8,
        ((len_or_head >> 16) & 0xFF) as u8,
        ((len_or_head >> 8) & 0xFF) as u8,
        (len_or_head & 0xFF) as u8,
    ];
    match verb_tag {
        0 => {
            if subject_len != 0 || len_or_head > MAX_RECORD_PAYLOAD as u64 {
                return None;
            }
        }
        1..=3 => {
            if subject_len == 0 || len_or_head > MAX_RECORD_PAYLOAD as u64 {
                return None;
            }
        }
        4 => {
            if subject_len != 0 {
                return None;
            }
        }
        _ => return None,
    }
    Some(out)
}

/// Decode a 24-byte `asb` frame header wire descriptor into
/// `(verb_tag, subject_len, offset_or_next, len_or_head)`.
#[must_use]
pub const fn asb_decode_frame_header_wire(buf: &[u8]) -> Option<(u8, u16, u64, u64)> {
    let [b'A', b'S', b'B', b'H', ASB_FRAME_HEADER_WIRE_VERSION, verb_tag, s0, s1, o0, o1, o2, o3, o4, o5, o6, o7, l0, l1, l2, l3, l4, l5, l6, l7, ..] =
        *buf
    else {
        return None;
    };
    let subject_len = ((s0 as u16) << 8) | (s1 as u16);
    let offset_or_next = ((o0 as u64) << 56)
        | ((o1 as u64) << 48)
        | ((o2 as u64) << 40)
        | ((o3 as u64) << 32)
        | ((o4 as u64) << 24)
        | ((o5 as u64) << 16)
        | ((o6 as u64) << 8)
        | (o7 as u64);
    let len_or_head = ((l0 as u64) << 56)
        | ((l1 as u64) << 48)
        | ((l2 as u64) << 40)
        | ((l3 as u64) << 32)
        | ((l4 as u64) << 24)
        | ((l5 as u64) << 16)
        | ((l6 as u64) << 8)
        | (l7 as u64);
    match verb_tag {
        0 => {
            if subject_len != 0 || len_or_head > MAX_RECORD_PAYLOAD as u64 {
                return None;
            }
        }
        1..=3 => {
            if subject_len == 0 || len_or_head > MAX_RECORD_PAYLOAD as u64 {
                return None;
            }
        }
        4 => {
            if subject_len != 0 {
                return None;
            }
        }
        _ => return None,
    }
    Some((verb_tag, subject_len, offset_or_next, len_or_head))
}

/// Four-byte magic prefix (`"ASBK"`) for a 24-byte `asb ack` wire header.
pub const ASB_ACK_WIRE_MAGIC: [u8; 4] = *b"ASBK";

/// Wire format version for `asb ack` wire headers.
pub const ASB_ACK_WIRE_VERSION: u8 = 1;

/// Encode a 24-byte `asb ack` wire header:
/// `[0..4] = b"ASBK"`, `[4] = version (1)`, `[5] =disposition_tag` (`0` = Handled,
/// `1` = Refused, `2` = Deferred), `[6..8] = group_len (BE)`,
/// `[8..16] = input_offset (BE)` (`< ACK_SEQ_BASE`),
/// `[16..24] = producer_seq (BE)` (`ACK_SEQ_BASE | input_offset`).
#[must_use]
pub const fn asb_encode_ack_wire(
    disposition_tag: u8,
    group_len: u16,
    input_offset: u64,
) -> Option<[u8; 24]> {
    let producer_seq = ACK_SEQ_BASE | input_offset;
    let out = [
        ASB_ACK_WIRE_MAGIC[0],
        ASB_ACK_WIRE_MAGIC[1],
        ASB_ACK_WIRE_MAGIC[2],
        ASB_ACK_WIRE_MAGIC[3],
        ASB_ACK_WIRE_VERSION,
        disposition_tag,
        ((group_len >> 8) & 0xFF) as u8,
        (group_len & 0xFF) as u8,
        ((input_offset >> 56) & 0xFF) as u8,
        ((input_offset >> 48) & 0xFF) as u8,
        ((input_offset >> 40) & 0xFF) as u8,
        ((input_offset >> 32) & 0xFF) as u8,
        ((input_offset >> 24) & 0xFF) as u8,
        ((input_offset >> 16) & 0xFF) as u8,
        ((input_offset >> 8) & 0xFF) as u8,
        (input_offset & 0xFF) as u8,
        ((producer_seq >> 56) & 0xFF) as u8,
        ((producer_seq >> 48) & 0xFF) as u8,
        ((producer_seq >> 40) & 0xFF) as u8,
        ((producer_seq >> 32) & 0xFF) as u8,
        ((producer_seq >> 24) & 0xFF) as u8,
        ((producer_seq >> 16) & 0xFF) as u8,
        ((producer_seq >> 8) & 0xFF) as u8,
        (producer_seq & 0xFF) as u8,
    ];
    if disposition_tag > 2 || group_len == 0 || input_offset >= ACK_SEQ_BASE {
        return None;
    }
    Some(out)
}

/// Decode a 24-byte `asb ack` wire header into `(disposition_tag, group_len, input_offset, producer_seq)`.
#[must_use]
pub const fn asb_decode_ack_wire(buf: &[u8]) -> Option<(u8, u16, u64, u64)> {
    let [b'A', b'S', b'B', b'K', ASB_ACK_WIRE_VERSION, disposition_tag, g0, g1, i0, i1, i2, i3, i4, i5, i6, i7, p0, p1, p2, p3, p4, p5, p6, p7, ..] =
        *buf
    else {
        return None;
    };
    let group_len = ((g0 as u16) << 8) | (g1 as u16);
    let input_offset = ((i0 as u64) << 56)
        | ((i1 as u64) << 48)
        | ((i2 as u64) << 40)
        | ((i3 as u64) << 32)
        | ((i4 as u64) << 24)
        | ((i5 as u64) << 16)
        | ((i6 as u64) << 8)
        | (i7 as u64);
    let producer_seq = ((p0 as u64) << 56)
        | ((p1 as u64) << 48)
        | ((p2 as u64) << 40)
        | ((p3 as u64) << 32)
        | ((p4 as u64) << 24)
        | ((p5 as u64) << 16)
        | ((p6 as u64) << 8)
        | (p7 as u64);
    if disposition_tag > 2
        || group_len == 0
        || input_offset >= ACK_SEQ_BASE
        || producer_seq != (ACK_SEQ_BASE | input_offset)
    {
        return None;
    }
    Some((disposition_tag, group_len, input_offset, producer_seq))
}

/// Decode one ASCII hex digit into its 4-bit value (`0..=15`).
const fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some((b - b'a').saturating_add(10)),
        b'A'..=b'F' => Some((b - b'A').saturating_add(10)),
        _ => None,
    }
}

/// Decode exactly `out.len()` bytes of hex from `hex`. Strict: ASCII hex digits
/// only (no sign, no whitespace, no multibyte), exact length. Byte-indexed, so a
/// multibyte character can never hit a char-boundary panic — it is simply not hex.
fn decode_hex(hex: &[u8], out: &mut [u8]) -> Result<(), String> {
    let Some(expected) = out.len().checked_mul(2) else {
        return Err("hex output length overflow".to_string());
    };
    if hex.len() != expected {
        return Err(format!(
            "expected {expected} hex chars ({} bytes), got {} bytes",
            out.len(),
            hex.len()
        ));
    }
    let mut i = 0usize;
    while i < out.len() {
        let pos = i.saturating_mul(2);
        let (Some(&h), Some(&l), Some(slot)) =
            (hex.get(pos), hex.get(pos.saturating_add(1)), out.get_mut(i))
        else {
            return Err(format!("not valid hex at char {pos}"));
        };
        match (hex_nibble(h), hex_nibble(l)) {
            (Some(hi), Some(lo)) => *slot = (hi << 4) | lo,
            _ => return Err(format!("not valid hex at char {pos}")),
        }
        i = i.saturating_add(1);
    }
    Ok(())
}

/// Where secret material comes from. Never argv — a `ps` from any same-uid process
/// would show it for the whole lifetime of the command.
enum KeySource {
    Env(String),
    File(String),
}

/// Read raw secret material from its source, labelled by `flag` (`--key`,
/// `--secret`) for the error message. Resolved once per invocation via
/// `Cli::transport` (`OnceCell`), so multi-connection verbs (`drain`) never
/// re-read the key source.
///
/// Not `std::env::var`: its `NotUnicode` error renders the variable's VALUE, so a
/// message built from it would print the secret. The refusal here names the
/// variable only.
fn load_bytes(flag: &str, src: &KeySource) -> Result<(String, Vec<u8>), u8> {
    match src {
        KeySource::Env(name) => {
            let Some(v) = std::env::var_os(name) else {
                return Err(fail_exit(
                    2,
                    &format!("{flag}-env {name}: environment variable not found"),
                ));
            };
            let Ok(v) = v.into_string() else {
                return Err(fail_exit(
                    2,
                    &format!("{flag}-env {name}: environment variable is not valid UTF-8"),
                ));
            };
            Ok((format!("{flag}-env {name}"), v.into_bytes()))
        }
        KeySource::File(path) => match std::fs::read(path) {
            Ok(v) => Ok((format!("{flag}-file {path}"), v)),
            Err(e) => Err(fail_exit(2, &format!("{flag}-file {path}: {e}"))),
        },
    }
}

/// How a `--key-file` is read. `serve` holds the key it SERVES under to the
/// owner-only secret-file rule — a daemon that runs for weeks on a group-readable
/// key has been handing it out the whole time — while a client verb reads its copy
/// as it finds it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KeyFile {
    /// Read the file as it is (a client's copy of the key).
    AsFound,
    /// Read it through [`capfile::read_private_file`]: refused unless 0600 or
    /// tighter, the mode read off the descriptor the key bytes come from. A check
    /// by path and then a read by path would resolve the path twice, and whoever
    /// can write the key's directory could swap a 0600 file for a link to a 0644
    /// one in between: `serve` would then seal its listener under a key that
    /// account chose and knows, and nothing would say so.
    OwnerOnly,
}

/// Read the sealed transport's pre-shared key, decode its 64 hex chars, and scrub
/// the intermediate buffer. Whitespace around the hex (a trailing newline in a key
/// file) is fine. `rule` says whether a `--key-file` must be owner-only.
fn load_key(src: &KeySource, rule: KeyFile) -> Result<[u8; 32], u8> {
    let (what, mut raw) = match (src, rule) {
        (KeySource::File(path), KeyFile::OwnerOnly) => match capfile::read_private_file(path) {
            Ok(v) => (format!("--key-file {path}"), v),
            // capfile's refusal already starts with the path: prefix only the flag.
            Err(e) => return Err(fail_exit(2, &format!("--key-file {e}"))),
        },
        _ => load_bytes("--key", src)?,
    };
    let mut key = [0u8; 32];
    let res = decode_hex(raw.trim_ascii(), &mut key);
    raw.fill(0);
    std::hint::black_box(&raw);
    if let Err(e) = res {
        return Err(fail_exit(2, &format!("{what}: {e}")));
    }
    Ok(key)
}

fn metadata_str(path: &str) -> io::Result<std::fs::Metadata> {
    std::fs::metadata(path)
}

fn remove_file_str(path: &str) -> io::Result<()> {
    std::fs::remove_file(path)
}

fn open_with_options(opts: &std::fs::OpenOptions, path: &str) -> io::Result<std::fs::File> {
    opts.open(path)
}

fn rename_str(from: &str, to: &str) -> io::Result<()> {
    std::fs::rename(from, to)
}

/// Read the MINT secret — arbitrary bytes, not hex, because that is what
/// `Broker::open_guarded` takes. `mint` and `serve` read it through this one
/// function, so the capability a mint seals is the one the guard verifies.
///
/// ONE RULE FOR A SECRET FILE, AND IT IS THE AUTHORITY'S: its bytes, exactly. A
/// mint secret is `head -c 32 /dev/urandom` — raw random bytes, which begin or end
/// with a whitespace byte about one time in twenty-five — and every other reader of
/// the same file (the aterm fabric's broker and mint, any embedder calling
/// `Broker::open_guarded` with `fs::read`) seals with every byte. Trimming here made
/// `asb mint` seal a DIFFERENT secret from the broker's for those files, silently.
/// A `--secret-file` is read by [`capfile::read_secret_file`], which holds it to
/// the whole rule (owner-only, exact bytes, at least [`capfile::SECRET_MIN`]); this
/// binary no longer keeps a copy of that rule. An environment variable is text and
/// cannot hold arbitrary bytes, so `--secret-env` is trimmed and then held to the
/// same floor here; write a text secret to a file with `printf %s`, not `echo`,
/// and the two sources agree.
fn load_secret(src: &KeySource) -> Result<Vec<u8>, u8> {
    let (what, s) = match src {
        KeySource::File(path) => match capfile::read_secret_file(path) {
            Ok(s) => (format!("--secret-file {path}"), s),
            Err(e) => return Err(fail_exit(2, &format!("--secret-file {e}"))),
        },
        KeySource::Env(_) => {
            let (what, raw) = load_bytes("--secret", src)?;
            (what, raw.trim_ascii().to_vec())
        }
    };
    if s.is_empty() {
        return Err(fail_exit(2, &format!("{what}: the mint secret is empty")));
    }
    if s.len() < capfile::SECRET_MIN {
        return Err(fail_exit(
            2,
            &format!(
                "{what}: the mint secret is {} bytes; it must be at least {} (a short key mints capabilities that seal nothing)",
                s.len(),
                capfile::SECRET_MIN
            ),
        ));
    }
    Ok(s)
}

/// Whether every address a `<host>:<port>` endpoint resolves to is LOOPBACK — the
/// only kind `serve` binds without `--allow-remote`. `0.0.0.0` and `[::]` listen on
/// every interface, so they are not.
fn is_loopback_endpoint(ep: &str) -> io::Result<bool> {
    use std::net::ToSocketAddrs;
    let mut any = false;
    let mut all_loopback = true;
    for addr in ep.to_socket_addrs()? {
        any = true;
        if !addr.ip().is_loopback() {
            all_loopback = false;
        }
    }
    if !any {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{ep} resolves to no address"),
        ));
    }
    Ok(all_loopback)
}

/// One minted capability to attach before the verb runs: the grant string and the
/// tag that seals it. Read from a `--cap-file`, never from argv.
type Cap = capfile::Line;

/// Read every `--cap-file`, in order: `<grant> <tag-hex>` per line, as `asb mint`
/// prints it, so a node's whole ring is one file.
///
/// The rules are `astream_cap::capfile`'s, and the reasons are documented there:
/// blank lines and `#` comments are skipped; each line splits at its LAST ASCII
/// whitespace, not its first, because a grant may legally hold a space (a
/// first-whitespace split made `ro:/f/F/pub a/>` unreadable and took the whole ring
/// down with it); the tag is exactly 64 hex digits.
///
/// A file that cannot be read exits 2 naming it; a malformed line is a usage error
/// (exit 2) naming `file:line`.
fn load_caps(paths: &[String]) -> Result<Vec<Cap>, u8> {
    let mut caps = Vec::new();
    for path in paths {
        match capfile::read_file(path) {
            Ok(ring) => caps.extend(ring),
            Err(e @ capfile::ReadError::Io { .. }) => {
                return Err(fail_exit(2, &format!("--cap-file {e}")));
            }
            Err(capfile::ReadError::Malformed(m)) => {
                return Err(usage_error(&format!("--cap-file {m}")));
            }
        }
    }
    Ok(caps)
}

/// The producer id a bound grant forces, when the ring names exactly one
/// principal. A `Publish` under a bound grant must carry precisely this id, so
/// deriving it is strictly better than making the operator retype it — and a ring
/// naming two principals is ambiguous, so that asks for an explicit `--id`.
///
/// Only a WRITABLE grant votes. `ro,p=<principal>:<filter>` parses, and the broker
/// deliberately binds nothing for it ("a read-only grant publishes nothing, so it
/// binds nothing"), so counting it would be wrong in both directions: it could
/// choose an id no writable grant on the ring may publish under, and it could make
/// an unambiguous ring look ambiguous and demand an `--id` for no reason.
#[cfg(feature = "cap")]
fn bound_producer_id(caps: &[Cap]) -> Result<Option<u64>, u8> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = caps;
    while let [c, tail @ ..] = rest {
        if let Ok(grant) = astream_cap::Grant::parse(c.grant.as_str()) {
            if matches!(grant.mode, astream_cap::Mode::ReadWrite) {
                if let Some(p) = grant.principal {
                    names.push(p);
                }
            }
        }
        rest = tail;
    }
    names.sort();
    names.dedup();
    if names.is_empty() {
        Ok(None)
    } else if let [only] = names.as_slice() {
        Ok(Some(astream_cap::producer_id_of(only.as_str())))
    } else {
        Err(usage_error(&format!(
            "the attached grants bind {} different principals ({}); pass --id to say which producer id to publish under",
            names.len(),
            names.join(", ")
        )))
    }
}

/// Without the `cap` feature nothing here can attach a capability at all, so
/// there is never a bound grant to derive a producer id from.
#[cfg(not(feature = "cap"))]
fn bound_producer_id(_caps: &[Cap]) -> Result<Option<u64>, u8> {
    Ok(None)
}

/// The CLIENT side of every transport is the library's: [`astream_broker::connect`]
/// takes the [`Transport`] resolved once per invocation (see `Cli::transport`) and
/// hands back one erased client per connection plus its [`Closer`] — a duplicated
/// descriptor onto the SAME socket, under any record layer, since `SO_RCVTIMEO` is a
/// property of the socket and not of the descriptor. The closer is what bounds
/// `drain`'s idle wait.
///
/// What asb adds is only the refusal a build without a transport's feature owes the
/// operator. The library answers `Unsupported` naming a cargo feature; asb names its
/// own flag and says which asb to build, as a USAGE error — exit 2, nothing sent —
/// and never as a silent fall back to plaintext. With no connect timeout, each
/// transport is its `Client` constructor exactly, as it was before the library had
/// one erased connect.
fn connect(t: &Transport, ep: &str) -> Result<(AnyClient, Closer), AsbErr> {
    match t {
        Transport::Unix if cfg!(not(unix)) => Err(AsbErr::Exit(fail_exit(
            2,
            "Unix-socket endpoints need a unix host; use --tcp",
        ))),
        Transport::Sealed(_) if cfg!(not(feature = "aead")) => Err(AsbErr::Exit(fail_exit(
            2,
            "--key-env/--key-file require asb built with `--features aead`",
        ))),
        Transport::Handshake(_) if cfg!(not(feature = "handshake")) => {
            Err(AsbErr::Exit(fail_exit(
                2,
                "--handshake requires asb built with `--features handshake`",
            )))
        }
        _ => Ok(astream_broker::connect(t, ep, None)?),
    }
}

// The SERVE side stays here: serving is not a client concern. Built without the
// feature, asb stays zero-third-party and a key source is a clear error rather than
// silent plaintext.
#[cfg(feature = "aead")]
fn serve_sealed(broker: &Broker, ep: &str, key: &[u8; 32]) -> Result<io::Result<BrokerHandle>, u8> {
    Ok(broker.serve_tcp_sealed(ep, *key))
}
#[cfg(not(feature = "aead"))]
fn serve_sealed(
    _broker: &Broker,
    _ep: &str,
    _key: &[u8; 32],
) -> Result<io::Result<BrokerHandle>, u8> {
    Err(fail_exit(
        2,
        "--key-env/--key-file require asb built with `--features aead`",
    ))
}

// Forward-secret handshake entry points, gated on the `handshake` feature (which
// implies `aead`, so parse_key_hex is available here).
#[cfg(feature = "handshake")]
fn serve_handshake(
    broker: &Broker,
    ep: &str,
    key: &[u8; 32],
) -> Result<io::Result<BrokerHandle>, u8> {
    Ok(broker.serve_tcp_handshake(ep, *key))
}
#[cfg(not(feature = "handshake"))]
fn serve_handshake(
    _broker: &Broker,
    _ep: &str,
    _key: &[u8; 32],
) -> Result<io::Result<BrokerHandle>, u8> {
    Err(fail_exit(
        2,
        "--handshake requires asb built with `--features handshake`",
    ))
}

struct Cli {
    pos: Vec<String>,
    bools: HashSet<String>,
    kv: HashMap<String, String>,
    /// The transport, resolved ONCE per invocation. `drain` opens two connections,
    /// so one resolution serves every connection.
    transport: std::cell::OnceCell<Transport>,
    /// `--cap-file` is the one REPEATABLE flag: a connection's keyring holds up to
    /// `MAX_KEYRING` grants, and reading one subtree while committing under another
    /// is the intended shape.
    cap_files: Vec<String>,
}

/// Flags that take a value (`--flag VALUE` or `--flag=VALUE`), given at most once.
const VALUE_FLAGS: &[&str] = &[
    "--id",
    "--seq",
    "--seq-file",
    "--from",
    "--after",
    "--max",
    "--idle",
    "--group",
    "--key-env",
    "--key-file",
    "--secret-env",
    "--secret-file",
    "--revocation-file",
    "--unix",
    "--durability",
    "--horizon-ms",
    "--health-ms",
    "--max-retained-records",
    "--max-retained-bytes",
    "--retain-file",
    "--retention-interval-ms",
];

/// Flags that take no value.
const BOOL_FLAGS: &[&str] = &[
    "--tcp",
    "--handshake",
    "--peek",
    "--legacy-unbound",
    "--allow-remote",
];

/// Flags every network verb accepts: how to reach the broker, and what to attach.
const COMMON_FLAGS: &[&str] = &[
    "--tcp",
    "--handshake",
    "--key-env",
    "--key-file",
    "--cap-file",
];

/// Flags REFUSED on argv, with the reason and the way to pass the value instead.
/// argv is visible to every same-uid process through `ps` for the whole lifetime
/// of the command, so a secret must never appear there.
const ARGV_SECRETS: &[(&str, &str)] = &[
    ("--key", "pass it with --key-env NAME or --key-file PATH"),
    (
        "--secret",
        "pass it with --secret-env NAME or --secret-file PATH",
    ),
    (
        "--cap-tag",
        "a capability tag is a secret; put `<grant> <tag-hex>` in a file and pass --cap-file PATH",
    ),
    (
        "--cap-filter",
        "a capability is now presented as one `<grant> <tag-hex>` line; pass --cap-file PATH",
    ),
];

/// Every line `asb` puts on STDOUT goes through here, and every one of them is
/// checked.
///
/// `println!` PANICS on a stdout write error — `exit 101` with a Rust backtrace on
/// stderr. That is none of the three statuses this binary's exit contract
/// enumerates (0 done, 1 runtime failure, 2 usage), so a supervisor that reads 2 as
/// "bad config, do not retry" and 1 as "transient, retry" can classify neither, and
/// for `pub` the retry it guesses at republishes under a fresh default sequence and
/// puts the record on the bus twice. The record-printing verbs surface the same
/// failure through `writeln!` + `?`; this is that path for the verbs that print one
/// line.
///
/// The flush is part of it: a buffered line that fails on the implicit flush at exit
/// would otherwise be a write error nobody sees at all.
fn say(line: &str) -> io::Result<()> {
    let mut out = stdout_buffered();
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

/// [`say`] for a line that reports a change the broker has ALREADY made durable —
/// the offset `pub`/`ack` published at, the group `commit` advanced. The durable
/// effect happened; only the report failed. So the fact is repeated on stderr
/// (where it survives a closed stdout, which is the ordinary shape of this failure:
/// `asb pub ... | head -1`), with the one instruction the exit status cannot carry —
/// that a retry would DUPLICATE, because `pub`'s default sequence is a fresh
/// timestamp. Then the error propagates and the process exits 1.
fn say_durable(line: &str, what: &str) -> io::Result<()> {
    match say(line) {
        Ok(()) => Ok(()),
        Err(e) => {
            warn(&format!(
                "asb: stdout: {e} — {what} anyway: `{line}`. Do NOT retry on this status: the effect is already durable"
            ));
            Err(e)
        }
    }
}

/// Every diagnostic `asb` puts on STDERR goes through here.
///
/// Not `eprintln!`, which panics on a write error exactly as `println!` does: a run
/// whose stderr is ALSO gone (`asb fetch ... 2>&1 | head -0`) would exit 101 out of
/// the path reporting a clean exit 1. A diagnostic that cannot be delivered is
/// DROPPED: the exit status is the part of the message that always survives, and it
/// is the part a supervisor reads.
fn warn(msg: &str) {
    let mut err = io::stderr().lock();
    let _ = err.write_all(msg.as_bytes());
    let _ = err.write_all(b"\n");
    let _ = err.flush();
}

fn usage_error(msg: &str) -> u8 {
    warn(&format!("asb: {msg}\n{USAGE}"));
    2
}

fn parse() -> Result<Cli, u8> {
    // `args_os`, NOT `args`: `std::env::args()` PANICS on an argument that is not
    // valid Unicode — exit 101 with a backtrace, before any of this strict-flag
    // handling runs. A non-UTF-8 `--key-file` path is a plausible real input on any
    // filesystem that permits one, and a malformed argument is a usage error.
    let mut args: Vec<String> = Vec::new();
    for a in std::env::args_os() {
        match a.into_string() {
            Ok(s) => args.push(s),
            Err(bad) => {
                return Err(usage_error(&format!(
                    "argument is not valid UTF-8: {bad:?}"
                )));
            }
        }
    }
    let mut pos = Vec::new();
    let mut bools: HashSet<String> = HashSet::new();
    let mut kv: HashMap<String, String> = HashMap::new();
    let mut cap_files: Vec<String> = Vec::new();
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        // `--` ends flag parsing: every token after it is a positional. Without it a
        // path/subject/group beginning with `-` is unusable, because the strict
        // unknown-flag check below (rightly) refuses every dash-leading token.
        if a == "--" {
            pos.extend(it.cloned());
            break;
        }
        // `--flag=value` spelling: split once, then treat like `--flag value`.
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        // A boolean takes no value, so it never swallows the next token.
        if BOOL_FLAGS.contains(&flag) {
            if inline.is_some() {
                return Err(usage_error(&format!("{flag} takes no value")));
            }
            bools.insert(flag.to_string());
            continue;
        }
        if let Some((_, remedy)) = ARGV_SECRETS.iter().find(|(f, _)| *f == flag) {
            return Err(usage_error(&format!(
                "{flag} on argv is refused: the value would be visible to every local user via `ps` for the process's lifetime; {remedy}"
            )));
        }
        if flag == "--cap-file" || VALUE_FLAGS.contains(&flag) {
            let v = match inline {
                Some(v) => v,
                None => match it.next() {
                    Some(v) if !v.starts_with("--") => v.clone(),
                    _ => return Err(usage_error(&format!("{flag} expects a value"))),
                },
            };
            if flag == "--cap-file" {
                cap_files.push(v);
            } else if kv.insert(flag.to_string(), v).is_some() {
                return Err(usage_error(&format!("{flag} given more than once")));
            }
            continue;
        }
        // Anything else that looks like a flag is a mistyped one. It must NOT fall
        // through as a positional: `--psk`, `--Key`, `--form` would otherwise be
        // silently ignored (a plaintext publish where a sealed one was intended).
        // The message names the flag WITHOUT any `=value`: a mistyped secret flag
        // (`--psk=<key>`) carries the key there.
        if a.starts_with('-') && a.len() > 1 {
            let name = a.split_once('=').map_or(a.as_str(), |(name, _)| name);
            return Err(usage_error(&format!("unknown flag {name}")));
        }
        pos.push(a.clone());
    }
    Ok(Cli {
        pos,
        bools,
        kv,
        transport: std::cell::OnceCell::new(),
        cap_files,
    })
}

impl Cli {
    fn at(&self, i: usize, what: &str) -> Result<String, u8> {
        match self.pos.get(i) {
            Some(s) => Ok(s.clone()),
            None => Err(usage_error(&format!("missing <{what}>"))),
        }
    }

    fn has(&self, flag: &str) -> bool {
        self.bools.contains(flag)
    }

    /// Reject surplus positionals (a stray token is a mistake, not noise).
    fn no_more_than(&self, n: usize) -> Result<(), u8> {
        if let Some(extra) = self.pos.get(n) {
            return Err(usage_error(&format!("unexpected argument {extra:?}")));
        }
        Ok(())
    }

    /// Reject any flag this verb does not take. A flag that belongs to another verb
    /// is exactly as much of a mistake as a mistyped one — `asb pub --from 3` means
    /// its author expected something the command will not do.
    fn only(&self, verb: &str, allowed: &[&str]) -> Result<(), u8> {
        let mut given: Vec<&str> = Vec::new();
        for k in self.kv.keys() {
            given.push(k.as_str());
        }
        for b in &self.bools {
            given.push(b.as_str());
        }
        if !self.cap_files.is_empty() {
            given.push("--cap-file");
        }
        given.sort_unstable(); // a deterministic message when several are wrong
        let mut rest = given.as_slice();
        while let [f, tail @ ..] = rest {
            let mut ok = false;
            let mut al_rest = allowed;
            while let [a, al_tail @ ..] = al_rest {
                if a.as_bytes() == f.as_bytes() {
                    ok = true;
                    break;
                }
                al_rest = al_tail;
            }
            if !ok {
                return Err(usage_error(&format!("{f} does not apply to {verb}")));
            }
            rest = tail;
        }
        Ok(())
    }

    /// The verb's own flags, on top of the transport/capability set every network
    /// verb shares.
    fn only_net(&self, verb: &str, extra: &[&str]) -> Result<(), u8> {
        let mut allowed: Vec<&str> = COMMON_FLAGS.to_vec();
        allowed.extend_from_slice(extra);
        self.only(verb, &allowed)
    }

    /// A numeric flag, STRICTLY: absent → `None`; present but not an unsigned
    /// integer → usage error. A present-but-unparsable value must never fall back
    /// to a default (that would silently turn an explicit idempotent `--id/--seq`
    /// into a fresh unique one, or an explicit `--from` into 0).
    fn num(&self, flag: &str) -> Result<Option<u64>, u8> {
        match self.kv.get(flag) {
            None => Ok(None),
            Some(v) => match v.parse::<u64>() {
                Ok(n) => Ok(Some(n)),
                Err(_) => Err(usage_error(&format!(
                    "{flag} expects an unsigned integer, got {v:?}"
                ))),
            },
        }
    }

    /// The same, for a flag the wire carries as a `u32` (a page size).
    fn num32(&self, flag: &str) -> Result<Option<u32>, u8> {
        match self.kv.get(flag) {
            None => Ok(None),
            Some(v) => match v.parse::<u32>() {
                Ok(n) => Ok(Some(n)),
                Err(_) => Err(usage_error(&format!(
                    "{flag} expects an unsigned integer below 2^32, got {v:?}"
                ))),
            },
        }
    }

    fn pos_num(&self, i: usize, what: &str) -> Result<u64, u8> {
        let v = self.at(i, what)?;
        match v.parse::<u64>() {
            Ok(n) => Ok(n),
            Err(_) => Err(usage_error(&format!(
                "<{what}> expects an unsigned integer, got {v:?}"
            ))),
        }
    }

    fn key_source(&self) -> Result<Option<KeySource>, u8> {
        match (self.kv.get("--key-env"), self.kv.get("--key-file")) {
            (Some(_), Some(_)) => Err(usage_error(
                "--key-env and --key-file are mutually exclusive",
            )),
            (Some(e), None) => Ok(Some(KeySource::Env(e.clone()))),
            (None, Some(f)) => Ok(Some(KeySource::File(f.clone()))),
            (None, None) => Ok(None),
        }
    }

    /// Where the mint secret comes from, if one was given (`serve` guards only when
    /// it was; `mint` requires one).
    fn secret_source(&self) -> Result<Option<KeySource>, u8> {
        match (self.kv.get("--secret-env"), self.kv.get("--secret-file")) {
            (Some(_), Some(_)) => Err(usage_error(
                "--secret-env and --secret-file are mutually exclusive",
            )),
            (Some(e), None) => Ok(Some(KeySource::Env(e.clone()))),
            (None, Some(f)) => Ok(Some(KeySource::File(f.clone()))),
            (None, None) => Ok(None),
        }
    }

    /// How this invocation reaches the broker — resolved on first use and CACHED,
    /// because `drain` opens two connections. A client's `--key-file` is read as
    /// found; `serve` resolves its own through [`Cli::transport_reading`] first.
    fn transport(&self) -> Result<&Transport, u8> {
        self.transport_reading(KeyFile::AsFound)
    }

    /// [`Cli::transport`], reading a `--key-file` under `rule`. The FIRST call
    /// resolves and caches, so `serve` calls this with [`KeyFile::OwnerOnly`]
    /// before anything asks for the transport.
    fn transport_reading(&self, rule: KeyFile) -> Result<&Transport, u8> {
        if let Some(t) = self.transport.get() {
            return Ok(t);
        }
        let handshake = self.has("--handshake");
        let resolved = match self.key_source()? {
            // A key implies TCP (sealing a local Unix socket is pointless).
            Some(src) if handshake => Transport::Handshake(Box::new(load_key(&src, rule)?)),
            Some(src) => Transport::Sealed(Box::new(load_key(&src, rule)?)),
            // `--handshake` without a key has nothing to authenticate the agreement:
            // an unauthenticated ephemeral DH is exactly what a MITM wants.
            None if handshake => {
                return Err(usage_error(
                    "--handshake needs a key (--key-env NAME or --key-file PATH)",
                ));
            }
            None if self.has("--tcp") => Transport::Tcp,
            None => Transport::Unix,
        };
        let _ = self.transport.set(resolved);
        self.transport.get().ok_or(1)
    }

    fn caps(&self) -> Result<Vec<Cap>, u8> {
        load_caps(&self.cap_files)
    }
}

// Nanoseconds since the epoch — a per-invocation-unique default producer_seq.
fn now_nanos() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as u64,
        Err(_) => 0,
    }
}

// Milliseconds since the epoch — the fabric envelope's informational `t=`.
fn now_millis() -> u64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as u64,
        Err(_) => 0,
    }
}

/// Connect over the chosen transport and attach every capability, in order, under
/// one `Hello`.
fn connect_attached(cli: &Cli, ep: &str, caps: &[Cap]) -> Result<(AnyClient, Closer), AsbErr> {
    // Validate every flag (usage errors) before touching the network.
    let transport = cli.transport()?;
    let (mut c, closer) = connect(transport, ep)?;
    c.attach_all(caps.iter().map(|cap| (cap.grant.as_str(), &cap.tag[..])))?;
    Ok((c, closer))
}

/// The producer id to publish under: an explicit `--id`, else the one a single
/// bound grant forces, else a per-invocation default. `require_stable` is set by
/// the verbs whose dedup key must survive a restart (`--seq`, `--seq-file`, `ack`): for
/// those the pid default is not merely unhelpful, it silently breaks the
/// exactly-once property the caller asked for, so it is an error instead.
fn producer_id(cli: &Cli, caps: &[Cap], require_stable: Option<&str>) -> Result<u64, u8> {
    if let Some(id) = cli.num("--id")? {
        return Ok(id);
    }
    if let Some(id) = bound_producer_id(caps)? {
        return Ok(id);
    }
    if let Some(why) = require_stable {
        return Err(usage_error(&format!(
            "{why} needs a producer id that survives a restart: pass --id N, or attach a grant bound to a principal (--cap-file)"
        )));
    }
    Ok(u64::from(std::process::id()))
}

fn do_pub(mut c: AnyClient, id: u64, seq: u64, subject: &str, body: &[u8]) -> io::Result<()> {
    let (offset, dup) = c.publish(id, seq, subject, body)?;
    let (what, line) = if dup {
        ("the record was already on the log", format!("{offset} dup"))
    } else {
        ("the record IS on the log", format!("{offset} new"))
    };
    say_durable(&line, what)
}

/// Record output goes through a `BufWriter`: stdout is line-buffered, so written
/// directly every header and every body's trailing newline is a write syscall of its
/// own. Buffered, a whole page is a handful of writes and a tailed record is one.
fn stdout_buffered() -> io::BufWriter<io::StdoutLock<'static>> {
    io::BufWriter::new(io::stdout().lock())
}

fn do_sub(mut sub: AnySubscription) -> io::Result<()> {
    let mut out = stdout_buffered();
    while let Some(event) = sub.recv_event()? {
        match event {
            Event::Delivery { offset, body, .. } => {
                writeln!(out, "{offset} {}", body.len())?;
                out.write_all(&body)?;
                out.write_all(b"\n")?;
                out.flush()?;
            }
            Event::Gap(gap) => warn_gap("sub", &gap),
            Event::Mark { .. } => {}
        }
    }
    Ok(())
}

/// WHAT RETENTION GAVE UP below a tail (`REQUIREMENTS-fabric-operations` R4, addendum A9),
/// one line on STDERR: `asb: <verb>: GAP from=<F> to=<T> records=<N|unknown>`. Every record
/// of the filter in `F..T` the read is not delivered was given up (`records`: how many,
/// where the broker knows). Not on stdout, whose framing a consumer parses byte for byte:
/// `sub` writes it before any record the tail delivers after it, `drain` before the batch.
fn warn_gap(verb: &str, gap: &Gap) {
    let records = match gap.records {
        Some(n) => n.to_string(),
        None => "unknown".to_string(),
    };
    warn(&format!(
        "asb: {verb}: GAP from={} to={} records={records}",
        gap.from, gap.to
    ));
}

/// The wildcard-read framing: `<offset> <nbytes> <subject>\n<body>\n`. `sub` omits
/// the subject because it tails one filter its caller already chose; `last`,
/// `fetch` and `drain` are subtree queries whose answers span subjects, and a
/// consumer that had to guess which subject a record came from could not route it.
///
/// The COUNT precedes the SUBJECT because the subject is attacker-chosen bytes: the
/// wire rejects control bytes in a subject but not 0x20, so a publisher holding a
/// legitimate write grant on a lane can publish to `/f/F/in/n-a1/evil 999` and, in
/// an `<offset> <subject> <nbytes>` header, hand every consumer that splits on
/// spaces a byte count of its choosing — desynchronizing the rest of the batch.
/// With both numeric fields first the parse is exact and unspoofable: read
/// `<offset>`, read `<nbytes>`, take the SUBJECT as the rest of the line (it can
/// never contain the newline that ends it), then exactly `<nbytes>` body bytes and
/// one newline.
fn print_records(out: &mut impl Write, records: &[Record]) -> io::Result<()> {
    for (offset, subject, body) in records {
        writeln!(out, "{offset} {} {subject}", body.len())?;
        out.write_all(body)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// `last`, bounded by the CALLER's `--max` rather than by the BROKER's own limits.
///
/// One `Last` request returns at most `LAST_PAGE_MAX` rows and visits at most
/// `LAST_SCAN_MAX` subject-index entries, matched or not — bounds the operator never
/// asked for and cannot see on stdout. A page either of them cut short looks exactly
/// like a complete answer (and when the scan bound ran out before anything matched,
/// there is not even a last subject to guess a cursor from), so this follows the
/// broker's resume cursor until it has `max` rows or the cursor says the filter's
/// range is exhausted.
///
/// The answer is therefore the UNION of several per-request snapshots, each pinned at
/// its own, later head. `next`/`head` are the FIRST request's — the lowest pin, and the
/// only one a paired `Subscribe` can start at with no gap — so a row from a later
/// request may carry an offset at or above the reported `head`. Rows cannot repeat
/// across requests (the cursor is a strictly increasing subject), but a tailing reader
/// must fold newest-wins. And a subject whose newest record lands at or above a
/// request's pin is OMITTED from that page: its displacing record arrives only on the
/// paired `sub --from <next>`, which is why the completeness contract (module doc) is
/// `--max` PLUS that `sub`, never `--max` alone.
///
/// THE LOOP IS THE LIBRARY'S: [`Client::last_walk`](astream_broker::Client::last_walk), the one place that knows how a
/// `Last` answer ends. asb hands it `--after` and `--max` as they came — its `max` is a
/// bound on the WHOLE walk's rows and each request asks for exactly the rows still
/// owed, which is the request sequence this function used to make by hand — and prints
/// what it collected only once the walk has ended, so an answer that fails part-way
/// prints no rows at all rather than a prefix that looks finished. The continuation
/// the walk hands back is not printed: on this face it is `--after <the last subject
/// printed>`, which the operator already has.
///
/// WHAT IT IS NOT IS ONE SNAPSHOT. Each `Last` request pins its own visible head
/// (`broker::last_page` pins it on the first round) and reads every scan round of THAT
/// request against it, so one request is a snapshot; the answer this returns is the
/// UNION of `n` of them, each pinned at its own, later head. `next`/`head` are the
/// FIRST request's, never a later one's, because that is the lowest of the pins and so
/// the only one a paired `Subscribe` can start at with no gap — from a later head, a
/// record that superseded a value an earlier request already reported would never be
/// delivered. The consequence a caller must know: A ROW FROM REQUEST 2..n MAY CARRY AN
/// OFFSET AT OR ABOVE THE REPORTED `head`, and the reported head is the safe resume
/// point rather than the head the whole answer was read at. Rows cannot REPEAT across
/// the requests — the cursor is a strictly increasing subject — but a subject may be
/// reported at a value the tail from `next` then supersedes, so a reader that tails
/// must fold newest-wins.
///
/// HONEST BOUNDARY — A SECOND, UNDISCLOSED REASON A PAGE CAN BE SHORT. The pin's
/// deliberate cost is at the store: `last_matching` decides a subject from its LATEST
/// offset and skips it when that offset is not below the pinned head. So a subject
/// whose newest record lands at or above a request's pin before the walk reaches it is
/// OMITTED from that request's page rather than returned at its older value. The
/// broker pays nothing for that because its `Mark.next` IS the pin and the displacing
/// record arrives on the reader's paired `Subscribe { from: next }` — but `asb last`
/// makes no paired read. It prints rows and a MARK and exits, `resume` comes back
/// empty (the range genuinely ran out), and the page is simply one row shorter with
/// nothing on stdout to say so. A caller that reads a short page as "this is the whole
/// answer" — a fleet roster, say — can therefore be one live subject short. THE
/// COMPLETENESS CONTRACT IS `--max` PLUS A `sub --from <next>`, not `--max` alone.
///
/// HONEST BOUNDARY: this walks the index at the broker's scan bound per round trip,
/// so a filter that matches sparsely over a very large subtree costs several round
/// trips. That is the price of an answer neither of the broker's own bounds cut short,
/// and each round trip still holds the broker's log lock only for its own bounded scan.
/// The walk is also LIVENESS-bounded, by the library's `LAST_WALK_PAGES_MAX` (4096)
/// requests: an answer that needs more — at the broker's 4096 rows a page, more than
/// 16 777 216 rows — is an error (no rows printed, exit 1), where the hand-rolled
/// loop this replaced had no bound and would have kept asking.
fn last_all(
    c: &mut AnyClient,
    filter: &str,
    after: &str,
    max: u32,
) -> io::Result<(Vec<Record>, u64, u64)> {
    let mut out: Vec<Record> = Vec::new();
    // The MARK is the FIRST request's; a `--max 0` head query makes exactly one.
    let ((next, head), _continue_after) = c.last_walk(filter, after, max, |row| {
        out.push(row);
        Ok(Walk::Continue)
    })?;
    Ok((out, next, head))
}

/// The broker's SUBSCRIBER-VISIBLE head, asked with `fetch`'s documented head query
/// (`max = 0`: the store returns before it scans a single record, so this costs one
/// round trip and no work). The head is EXCLUSIVE — it is the log's record count, the
/// offset the next append takes — so a real record's offset is always strictly below
/// it.
///
/// `filter` is what the request is AUTHORIZED against, and the callers pass the very
/// name the operation itself is authorized against — `commit`'s group, `ack`'s output
/// subject. That is not decoration. A guarded broker authorizes `Commit { group }` by
/// `grants_commit`, which needs a read-write grant whose filter MATCHES the group as a
/// subject, and it authorizes `Fetch { filter }` by `grants_filter`, which needs any
/// grant whose filter CONTAINS the requested one. For a wildcard-free name those two
/// are the same question, so this probe is authorized exactly when the operation it
/// guards is: it can never turn an allowed `commit` into an `unauthorized`, and it can
/// never read a head a caller was not entitled to ask for.
fn visible_head(c: &mut AnyClient, filter: &str) -> io::Result<u64> {
    let (_page, (_next, head)) = c.fetch(0, filter, 0)?;
    Ok(head)
}

/// Refuse a `<upto>`/`<offset>` that names no record — a consumer group's committed
/// offset is MONOTONE by design, so an over-large one is a durable fact with no way
/// back.
///
/// `BrokerLog::commit` appends whatever `upto` it is handed and `group_start` is
/// `upto + 1`, clamped against nothing; no asb verb and no broker request lowers a
/// group's cursor. An over-large operand — a wrapper interpolating a stale or unset
/// shell variable — would leave the group skipped past every record it had not read,
/// for good: the only recovery is abandoning the group name, which in this fabric is
/// a fleet-wide config change.
///
/// The check is one-sided and cannot go wrong the other way: the head only ever
/// GROWS, so an `upto` below the head when it was read is still below the head when
/// the commit lands. The window it declines to serve is an operand that names a
/// record the broker does not have yet, which is exactly the input this refuses.
fn refuse_past_head(what: &str, value: u64, head: u64) -> Result<(), u8> {
    if value >= head {
        return Err(usage_error(&format!(
            "<{what}> {value} is at or past the broker's visible head {head}: it names no record. A consumer group's committed offset is MONOTONE — nothing lowers it again — so committing past the head durably skips every record the group has not read, and the only recovery is a new group name"
        )));
    }
    Ok(())
}

/// A bounded read's answer: the page, then where to resume from.
fn print_page(records: &[Record], next: u64, head: u64) -> io::Result<()> {
    let mut out = stdout_buffered();
    print_records(&mut out, records)?;
    writeln!(out, "MARK next={next} head={head}")?;
    out.flush()
}

/// Advance a persisted producer sequence BY ONE and make that durable before the
/// caller publishes under it — the write-ahead that lets a bridge in any language
/// keep a safe sequence across restarts without holding a connection open.
///
/// A missing file starts at 0, so the first published sequence is 1. Every OTHER
/// shape of file that does not hold an unsigned integer is an ERROR — including an
/// EMPTY or whitespace-only one, which is what a truncating wrapper (`: > n.seq`), a
/// torn `tee`, or a crashed writer leaves behind. Guessing 0 there would silently
/// restart the sequence, and the broker would dedup the live records that follow
/// away as if they were retries: `asb pub` would print `<some old offset> dup` and
/// exit 0 while the record was never published. Only an ABSENT file starts a
/// sequence. The new value is written to a sibling temp file, fsynced, then renamed
/// over the original, so a crash mid-write leaves either the old value or the new
/// one, never a torn digit; the directory entry is fsynced too, and a failure there
/// is reported rather than swallowed (a lost rename would re-use a burned number,
/// which is the same silent dedup).
///
/// CONCURRENT INVOCATIONS ARE SERIALIZED. The read, the add and the write are ONE
/// critical section under [`lock_seq_file`]'s exclusive advisory lock, because they
/// are a read-modify-write on a shared file and two `asb pub --seq-file <same path>`
/// processes are the ordinary case (a bridge publishing two rows at once). Unlocked,
/// both read the same value, both publish under the same `(producer_id,
/// producer_seq)`, and the broker dedups the second away: `asb pub` prints
/// `<the first record's offset> dup` and exits 0 while the caller's bytes are never
/// on the bus — exactly the silent loss `--seq-file` exists to prevent. The staging
/// file is per-process too (`<path>.<pid>.new`), so even where the lock cannot be
/// taken two invocations never truncate and rename ONE shared inode out from under
/// each other.
///
/// HONEST BOUNDARY: the file advances BEFORE the publish, so a crash in between
/// BURNS that sequence number — the record is never published and nothing ever
/// re-uses the number. That is deliberate: the failure mode is a lost record, never
/// a duplicate one, which is the only side a producer key may fail on.
fn advance_seq_file(path: &str) -> io::Result<u64> {
    // Held until this function returns: the read below and the rename at the end are
    // one critical section, not two.
    let _lock = lock_seq_file(path)?;
    let current = match std::fs::read(path) {
        Ok(bytes) => {
            let trimmed = bytes.trim_ascii();
            let mut parsed = None;
            if let Ok(s) = std::str::from_utf8(trimmed) {
                if let Ok(n) = s.parse::<u64>() {
                    parsed = Some(n);
                }
            }
            match parsed {
                Some(n) => n,
                None => {
                    let shown = std::str::from_utf8(trimmed).unwrap_or("<non-utf8>");
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "--seq-file {path}: {shown:?} is not an unsigned integer (only an ABSENT \
                             file starts a sequence; an empty one would silently restart it and have \
                             the broker dedup live records away)"
                        ),
                    ));
                }
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e),
    };
    let next = current
        .checked_add(1)
        .filter(|n| *n < ACK_SEQ_BASE)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "--seq-file {path}: the sequence is exhausted (sequences at or above 2^63 \
                     are reserved for `asb ack`)"
                ),
            )
        })?;
    // A PER-PROCESS staging name: a peer invocation can never truncate it mid-write
    // or rename it away.
    let tmp = format!("{path}.{}.new", std::process::id());
    let staged = (|| {
        // Created FRESH (`create_new`: O_CREAT|O_EXCL), never opened through whatever
        // already has the name: in a shared directory that may be a planted symlink,
        // and a plain create truncates and overwrites the link's target. A leftover
        // from a crashed run under the same pid is unlinked first (unlinking a
        // symlink never touches its target).
        let _ = remove_file_str(&tmp);
        let mut f = open_with_options(
            std::fs::OpenOptions::new().write(true).create_new(true),
            &tmp,
        )?;
        f.write_all(next.to_string().as_bytes())?;
        astream_sys::full_sync(&f)?;
        drop(f);
        rename_str(&tmp, path)
    })();
    if let Err(e) = staged {
        // Leave no half-written staging file behind for the next invocation to trip
        // over; the sequence file itself is untouched, so nothing was burned.
        let _ = remove_file_str(&tmp);
        return Err(io::Error::new(
            e.kind(),
            format!("--seq-file {path}: staging {tmp}: {e}"),
        ));
    }
    sync_parent_dir(path)?;
    Ok(next)
}

/// Take the EXCLUSIVE advisory lock that makes [`advance_seq_file`]'s read-modify-
/// write atomic against every other `asb` invocation on the same sequence file.
///
/// The lock lives on a SIBLING `<path>.lock`, never on the sequence file itself: the
/// update replaces that file by rename, so a second process would be holding a lock
/// on the inode the first has already unlinked and the two would run concurrently on
/// different inodes. The sidecar is created once and never renamed or removed, so
/// every invocation locks the same inode.
///
/// BLOCKING, not `try_lock` (which is what [`BrokerLog`] wants, where a second holder
/// is a misconfiguration): contention here is two publishes a millisecond apart, the
/// case that must SERIALIZE rather than fail. The lock is held across a read, a
/// short write, two fsyncs and a rename. The OS releases it when the process dies, so
/// a crash never leaves a stale lock behind.
///
/// HONEST BOUNDARY: a filesystem with no advisory locking answers `Unsupported`, and
/// that is tolerated — the same judgement `store::lock_exclusive` makes for the log
/// lock. There, concurrent invocations are back to racing, and only the per-process
/// staging name protects them.
fn lock_seq_file(path: &str) -> io::Result<std::fs::File> {
    let lock_path = format!("{path}.lock");
    // Open the sidecar that is there; CREATE it only when nothing is, and then with
    // `create_new` (O_EXCL), which refuses a symlink at the name where a plain create
    // would follow it and make an empty file at the link's target. A concurrent
    // invocation that created it first is simply opened.
    let open = |create: bool| {
        open_with_options(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(create),
            &lock_path,
        )
    };
    let f: std::fs::File = match open(false) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => match open(true) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => open(false),
            other => other,
        },
        other => other,
    }
    .map_err(|e| {
        io::Error::new(
            e.kind(),
            format!("--seq-file {path}: open {lock_path}: {e}"),
        )
    })?;
    match f.lock() {
        Ok(()) => Ok(f),
        Err(e) if e.kind() == io::ErrorKind::Unsupported => Ok(f),
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("--seq-file {path}: lock {lock_path}: {e}"),
        )),
    }
}

/// fsync the directory that names the sequence file, so the RENAME survives a power
/// loss: without it a crash can leave the old value on disk, and the next invocation
/// re-uses a sequence number the broker has already seen — the record is deduped
/// away silently, which is exactly the loss `--seq-file` exists to prevent.
///
/// A filesystem that will not let this process fsync a directory HANDLE (Unsupported
/// / InvalidInput), or a directory it may write but not open, is not a reason to
/// refuse the publish — that is the same judgement `store::sync_parent_dir` makes.
/// Any other error IS reported: a directory that fails its own fsync has not made
/// the rename durable, and the caller must not publish under a number that may not
/// have been recorded.
fn sync_parent_dir(path: &str) -> io::Result<()> {
    let dir = match std::path::Path::new(path).parent() {
        Some(d) if !d.as_os_str().is_empty() => d.to_path_buf(),
        _ => std::path::PathBuf::from("."),
    };
    let res = match std::fs::File::open(&dir) {
        Ok(d) => astream_sys::full_sync(&d),
        Err(e) => Err(e),
    };
    match res {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::Unsupported
                    | io::ErrorKind::InvalidInput
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::NotFound
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(io::Error::new(
            e.kind(),
            format!("--seq-file {path}: fsync of {}: {e}", dir.display()),
        )),
    }
}

/// The filter half of `grant` when `grant` IS the unbound god cap — read-write with
/// no bound principal, which may publish under ANY producer id — else `None`.
///
/// The question is asked of the PARSER, never of the spelling. `Grant::parse` maps
/// both `/f/F/>` and `rw:/f/F/>` to exactly the same authority (`ReadWrite`, no
/// principal), so a gate on the leading `/` refuses the careless spelling and hands
/// the identical fleet root out under the other one — silently, at exit 0.
#[cfg(feature = "cap")]
fn unbound_god_cap_filter(grant: &str) -> Option<String> {
    let g = astream_cap::Grant::parse(grant).ok()?;
    (g.mode == astream_cap::Mode::ReadWrite && g.principal.is_none()).then_some(g.filter)
}

/// Without the `cap` feature there is no vetted parser here to ask — and no MAC
/// either, so `mint` exits 2 naming the feature before it could print a tag. The
/// careless spelling is still refused with the same message and the same guidance;
/// the semantic gate belongs to the build that can actually mint.
#[cfg(not(feature = "cap"))]
fn unbound_god_cap_filter(grant: &str) -> Option<String> {
    if matches!(grant.as_bytes().first(), Some(&b'/')) {
        let mut out = String::new();
        out.push_str(grant);
        Some(out)
    } else {
        None
    }
}

/// Mint a capability and print the one line a `--cap-file` holds.
#[cfg(feature = "cap")]
fn do_mint(secret: &[u8], grant: &str) -> Result<(), AsbErr> {
    let cap = match astream_cap::mint(secret, grant) {
        Ok(c) => c,
        Err(e) => return Err(AsbErr::Exit(usage_error(&format!("mint: {e}")))),
    };
    // Not `say_durable`: minting touches no broker and is deterministic, so a lost
    // line costs nothing but a re-run.
    Ok(say(&capfile::format_line(grant, &cap.tag))?)
}

/// Minting needs the same vetted MAC the broker verifies with.
#[cfg(not(feature = "cap"))]
fn do_mint(_secret: &[u8], _grant: &str) -> Result<(), AsbErr> {
    Err(AsbErr::Exit(fail_exit(
        2,
        "mint requires asb built with `--features cap`",
    )))
}

/// Attenuate a parent capability into a narrower child capability (`REQUIREMENTS` R6).
/// When `secret` is `None`, performs offline Macaroon attenuation (`attenuate_offline`)
/// using the parent capability's bearer tag without needing the broker's mint secret.
#[cfg(feature = "cap")]
fn do_attenuate(secret: Option<&[u8]>, parent: &Cap, child_grant: &str) -> Result<(), AsbErr> {
    let parent_cap = astream_cap::Capability {
        filter: parent.grant.clone(),
        tag: parent.tag,
    };
    let now = now_millis();
    let res = match secret {
        Some(secret) => astream_cap::attenuate(secret, &parent_cap, child_grant, now),
        None => astream_cap::attenuate_offline(&parent_cap, child_grant, now),
    };
    let child = match res {
        Ok(c) => c,
        Err(e) => return Err(AsbErr::Exit(usage_error(&format!("attenuate: {e}")))),
    };
    Ok(say(&capfile::format_line(&child.filter, &child.tag))?)
}

#[cfg(not(feature = "cap"))]
fn do_attenuate(_secret: Option<&[u8]>, _parent: &Cap, _child_grant: &str) -> Result<(), AsbErr> {
    Err(AsbErr::Exit(fail_exit(
        2,
        "attenuate requires asb built with `--features cap`",
    )))
}

/// Refuse to take over a Unix socket a live broker is answering on: a peer that
/// accepts the connection is a running broker, and hijacking its path would leave
/// two daemons on one log. (`Broker::serve` makes the same probe and refuses a path
/// that is not a socket; checking here names the endpoint in asb's own error.)
#[cfg(unix)]
fn refuse_live_socket(ep: &str) -> Result<(), u8> {
    if metadata_str(ep).is_ok() && std::os::unix::net::UnixStream::connect(ep).is_ok() {
        return Err(fail_exit(
            1,
            &format!("{ep} is already served by another broker (a peer answered on the socket); refusing to take it over"),
        ));
    }
    Ok(())
}
#[cfg(not(unix))]
fn refuse_live_socket(_ep: &str) -> Result<(), u8> {
    Ok(())
}

/// How often `serve` rewrites its health sidecar, `<log>.health`, unless `--health-ms`
/// says otherwise: `stats` on a live log reads facts at most this old (the poison is
/// written at once, whatever the period). A few hundred bytes renamed into place, never
/// flushed, every two seconds.
const SERVE_HEALTH_EVERY: Duration = Duration::from_secs(2);

/// `serve`'s tier: `--durability strict|ordered|relaxed` (strict when absent), and
/// `--horizon-ms N` for ordered alone (the default horizon, one second, when absent).
/// Strict like every other flag: an unknown tier, a horizon for another tier or a
/// non-numeric one is a usage error, never a silent default.
fn serve_durability(cli: &Cli) -> Result<Durability, u8> {
    let horizon_ms = cli.num("--horizon-ms")?;
    let word = match cli.kv.get("--durability") {
        Some(w) => w.as_str(),
        None => "strict",
    };
    if word == "ordered" {
        return match horizon_ms {
            None => Ok(Durability::ordered()),
            Some(ms) => Ok(Durability::Ordered {
                horizon: Some(Duration::from_millis(ms)),
            }),
        };
    }
    if word == "strict" || word == "relaxed" {
        if horizon_ms.is_some() {
            return Err(usage_error(
                "--horizon-ms applies to --durability ordered alone",
            ));
        }
        return if word == "strict" {
            Ok(Durability::Strict)
        } else {
            Ok(Durability::Relaxed)
        };
    }
    Err(usage_error(&format!(
        "--durability {word:?}: expected strict, ordered or relaxed"
    )))
}

/// A tier's name on the command line and in `serve`'s stderr.
fn tier_word(tier: astream_broker::Tier) -> &'static str {
    match tier {
        astream_broker::Tier::Strict => "strict",
        astream_broker::Tier::Ordered => "ordered",
        astream_broker::Tier::Relaxed => "relaxed",
        _ => "other",
    }
}

/// What an Ordered broker's horizon promises, for `serve`'s stderr: `, horizon N ms`, a
/// full flush after every batch for 0, and no bound at all for none (which an embedder's
/// broker can ask for and `asb` cannot); nothing for another tier, which keeps none.
fn horizon_note(served: &astream_broker::ServedInfo) -> String {
    if !matches!(served.requested, astream_broker::Tier::Ordered) {
        return String::new();
    }
    match served.horizon_ms {
        None => ", no horizon (nothing but a clean stop flushes in full)".to_string(),
        Some(0) => ", horizon 0 ms (a full flush after every batch)".to_string(),
        Some(ms) => format!(", horizon {ms} ms"),
    }
}

/// The broker `serve` runs: at `durability`, and GUARDED when there is a secret (every
/// attach must then carry a capability minted under it). No escalation table: `asb`
/// names no record a person would miss, so on the weaker tiers every record rides the
/// tier's own promise. `serve` refuses a secret in a build without `cap` before it
/// reads one, so the fallback below never sees `Some`.
///
/// `asb serve` keeps the default signal action (it has no signal code, and astream no
/// libc), so a SIGTERM ends it without the clean close a broker's shutdown runs: after a
/// reboot, an `ordered` or `relaxed` log `asb serve` served moves its generation once —
/// the safe direction, and what a reader holding offsets is told by the identity.
#[cfg(feature = "cap")]
fn open_broker(log: &str, secret: Option<Vec<u8>>, durability: Durability) -> io::Result<Broker> {
    let mut opts = astream_broker::OpenOptions::new(durability);
    opts.guard = secret;
    Broker::open_opts(log, opts, || Ok(astream_broker::Prepared::Nothing))
}
#[cfg(not(feature = "cap"))]
fn open_broker(log: &str, _secret: Option<Vec<u8>>, durability: Durability) -> io::Result<Broker> {
    Broker::open_with(log, durability)
}

/// A bind refused AFTER the log was opened must not leave the log behind when
/// this invocation is the one that created it: a stray empty `b2.log` from a
/// typo'd retry later reads as a real (empty) bus. A log that held anything, or
/// that existed before, is never touched.
fn discard_created_log(log: &str, created_here: bool, broker: Broker) {
    drop(broker);
    if created_here && matches!(metadata_str(log), Ok(m) if m.len() == 0) {
        let _ = remove_file_str(log);
    }
}

/// Open the log — GUARDED when a mint secret was given — translating "someone else
/// holds this log" into a clear exit 1.
fn open_log(log: &str, secret: Option<Vec<u8>>, durability: Durability) -> Result<Broker, u8> {
    match open_broker(log, secret, durability) {
        Ok(b) => Ok(b),
        Err(e) => {
            let msg = e.to_string().to_ascii_lowercase();
            let held = matches!(
                e.kind(),
                io::ErrorKind::WouldBlock
                    | io::ErrorKind::AlreadyExists
                    | io::ErrorKind::ResourceBusy
            ) || msg.contains("lock")
                || msg.contains("served");
            if held {
                Err(fail_exit(
                    1,
                    &format!("log {log} is already served by another broker ({e})"),
                ))
            } else {
                Err(fail_exit(1, &format!("open {log}: {e}")))
            }
        }
    }
}

fn refuse_serve(log: &str, created_here: bool, what: &str, e: &io::Error, broker: Broker) -> u8 {
    discard_created_log(log, created_here, broker);
    if e.kind() == io::ErrorKind::AddrInUse {
        fail_exit(
            1,
            &format!("{what} is already served by another broker ({e})"),
        )
    } else {
        fail_exit(1, &format!("serve {what}: {e}"))
    }
}

// The explicit, operator-invoked repair for a log `serve` refuses to open.
// `BrokerLog::open` fails closed on a corrupt record because everything from
// there on may be acked data; this is the one command that discards it, and it
// says how much it discarded. A clean or merely torn log is left as it is.
fn cmd_repair(cli: &Cli) -> Result<(), AsbErr> {
    let log = cli.at(1, "log")?;
    cli.no_more_than(2)?;
    if cli.has("--tcp") {
        return Err(AsbErr::Exit(usage_error(
            "--tcp does not apply to repair (it operates on a local log file)",
        )));
    }
    cli.only("repair", &[])?;
    let (_log, dropped) = BrokerLog::open_repair(&log, Durability::Strict)?;
    if dropped == 0 {
        Ok(say(&format!("{log}: nothing to repair"))?)
    } else {
        Ok(say_durable(
            &format!("{log}: repaired, dropped {dropped} byte(s) from the corrupt record on"),
            "the log WAS truncated",
        )?)
    }
}

// Inspect a local log's operational counters (`REQUIREMENTS` R5).
fn cmd_stats(cli: &Cli) -> Result<(), AsbErr> {
    let log = cli.at(1, "log")?;
    cli.no_more_than(2)?;
    if cli.has("--tcp") {
        return Err(AsbErr::Exit(usage_error(
            "--tcp does not apply to stats (it operates on a local log file)",
        )));
    }
    cli.only("stats", &[])?;
    if !std::path::Path::new(&log).exists() {
        return Err(AsbErr::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{log}: no such log file"),
        )));
    }
    // Strict, though nothing is appended: every open records the tier it serves
    // in `<log>.id`, and a Relaxed record left by an inspection would make the
    // first open after the next reboot move the log's generation for a run that
    // acked nothing. (The open's own fsync makes everything durable first, so a
    // Strict record here hides no loss.)
    let opened = match BrokerLog::open_with(&log, Durability::Strict) {
        Ok(opened) => opened,
        // A LIVE log: the broker serving it holds its lock, so this process cannot
        // open it. Its health sidecar says what it holds instead.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return live_stats(&log),
        Err(e) => return Err(AsbErr::Io(e)),
    };
    let s = opened.stats();
    Ok(say(&format!(
        "base={} head={} records={} bytes={} subjects={} producers={} wills={} groups={} replica={}",
        s.base.0,
        s.head.0,
        s.retained_records,
        s.retained_bytes,
        s.distinct_subjects,
        s.bound_producers,
        s.pending_wills,
        s.group_commits,
        if s.is_replica { "yes" } else { "no" }
    ))?)
}

// `stats` on a log a running broker serves: the snapshot that broker keeps in
// `<log>.health` (`asb serve` keeps one), in the offline line's words, then what only a
// running broker has — its heads, connections, parked tails, flushes, its pid and when
// it wrote, whether it is the broker serving now — and the poison LAST, so its reason
// is the rest of the line. Printed only while the snapshot can be current: when its
// writer still holds it (`writer=serving`), or when this host cannot tell
// (`writer=unknown`, judged by `pid` and `at_ms`). A snapshot whose writer is gone is
// a dead broker's last word about a log another process now holds: exit 1, naming it,
// nothing on stdout.
fn live_stats(log: &str) -> Result<(), AsbErr> {
    let read = match astream_broker::read_health(log)? {
        Some(read) => read,
        None => {
            return Err(AsbErr::Exit(fail_exit(
                1,
                &format!(
                    "{log} is served by a running broker that keeps no health sidecar \
                     ({log}{}); `asb serve` keeps one",
                    astream_broker::HEALTH_SUFFIX
                ),
            )))
        }
    };
    let writer = match read.writer {
        astream_broker::HealthWriter::Serving => "serving",
        astream_broker::HealthWriter::Unknown => "unknown",
        astream_broker::HealthWriter::Gone => {
            return Err(AsbErr::Exit(fail_exit(
                1,
                &format!(
                    "{log} is served by a running broker, but its health sidecar is the \
                     last word of pid {}, which no longer serves it: no current snapshot",
                    read.health.pid
                ),
            )))
        }
    };
    let h = read.health;
    let poisoned = match &h.poisoned {
        Some(why) => format!("yes {why}"),
        None => "no".to_string(),
    };
    Ok(say(&format!(
        "base={} head={} records={} bytes={} subjects={} producers={} wills={} groups={} \
         replica={} visible={} durable={} connections={} parked_tails={} served={} \
         flush_barrier={} flush_full={} flush_skipped={} flush_failed={} writer={writer} \
         pid={} at_ms={} poisoned={poisoned}",
        h.base,
        h.head,
        h.retained_records,
        h.retained_bytes,
        h.distinct_subjects,
        h.bound_producers,
        h.pending_wills,
        h.group_commits,
        if h.replica { "yes" } else { "no" },
        h.visible_head,
        h.durable_head,
        h.active_conns,
        h.parked_tails,
        tier_word(h.served.served),
        h.flushes.barrier,
        h.flushes.full,
        h.flushes.skipped,
        h.flushes.failed,
        h.pid,
        h.written_at_ms,
    ))?)
}

// State-carrying log compaction (`REQUIREMENTS` R4): compact `<log>` to `<min_offset>`,
// preserving producer bindings, pending wills, producer sequence fences, consumer-group
// commits, and the latest record of every quiet subject (`Last`) in `<log>.state`.
fn cmd_compact(cli: &Cli) -> Result<(), AsbErr> {
    let log = cli.at(1, "log")?;
    let min_offset = cli.pos_num(2, "min_offset")?;
    cli.no_more_than(3)?;
    if cli.has("--tcp") {
        return Err(AsbErr::Exit(usage_error(
            "--tcp does not apply to compact (it operates on a local log file)",
        )));
    }
    cli.only("compact", &[])?;
    if !cfg!(feature = "retention") {
        return Err(AsbErr::Exit(fail_exit(
            2,
            "compact requires asb built with `--features retention`",
        )));
    }
    if !std::path::Path::new(&log).exists() {
        return Err(AsbErr::Io(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{log}: no such log file"),
        )));
    }
    #[cfg(feature = "retention")]
    {
        let mut opened = BrokerLog::open_with(&log, Durability::Strict)?;
        let dropped = opened.retain_before_carrying_state(astream_wire::Offset(min_offset))?;
        let s = opened.stats();
        Ok(say_durable(
            &format!(
                "{log}: compacted, dropped {dropped} record(s); base={} head={}",
                s.base.0, s.head.0
            ),
            "the log WAS compacted",
        )?)
    }
    #[cfg(not(feature = "retention"))]
    {
        let _ = min_offset;
        Err(AsbErr::Exit(2))
    }
}

// Mint a capability line. Offline: it touches no broker, only the secret.
fn cmd_mint(cli: &Cli) -> Result<(), AsbErr> {
    let grant = cli.at(1, "grant")?;
    cli.no_more_than(2)?;
    cli.only(
        "mint",
        &["--secret-env", "--secret-file", "--legacy-unbound"],
    )?;
    // The mode AND the binding are the whole point of the grant string, so
    // `mint` will not guess them. The read-write grant that names no principal
    // IS a valid grant — the fleet-root god cap — which is exactly why minting
    // one is opt-in: the careless command must not be the dangerous one. The
    // gate reads the PARSED grant, because `/f/F/>` and `rw:/f/F/>` are one
    // authority in two spellings and gating on the spelling gates on nothing.
    let legacy = cli.has("--legacy-unbound");
    match (unbound_god_cap_filter(&grant), legacy) {
        (Some(filter), false) => {
            return Err(AsbErr::Exit(usage_error(&format!(
                "refusing to mint {grant:?}: it is the READ-WRITE, UNBOUND grant (it may publish under any producer id) — `{filter}` and `rw:{filter}` are the same authority in two spellings. Say the mode and the binding: \"ro:{filter}\" to read, \"rw,p=<principal>:{filter}\" to write as that principal — or --legacy-unbound if the god cap is really what you want"
            ))));
        }
        (None, true) => {
            return Err(AsbErr::Exit(usage_error(
                "--legacy-unbound applies only to the READ-WRITE, UNBOUND grant (a bare filter, or `rw:` with no `,p=<principal>`); this grant is already read-only or bound to a principal",
            )));
        }
        _ => {}
    }
    let Some(src) = cli.secret_source()? else {
        return Err(AsbErr::Exit(usage_error(
            "mint needs the broker's mint secret: --secret-env NAME or --secret-file PATH",
        )));
    };
    let secret = load_secret(&src)?;
    do_mint(&secret, &grant)
}

// Attenuate a parent capability line into a narrower child capability (`REQUIREMENTS` R6).
// When `--secret-env`/`--secret-file` is omitted, attenuates offline as a bearer Macaroon
// capability (`via=<hex-parent-grant>`); when provided, mints a fresh root capability.
fn cmd_attenuate(cli: &Cli) -> Result<(), AsbErr> {
    let child_grant = cli.at(1, "child-grant")?;
    cli.no_more_than(2)?;
    cli.only(
        "attenuate",
        &["--cap-file", "--secret-env", "--secret-file"],
    )?;
    let caps = cli.caps()?;
    if caps.len() != 1 {
        return Err(AsbErr::Exit(usage_error(&format!(
            "attenuate requires exactly 1 parent capability via --cap-file (got {})",
            caps.len()
        ))));
    }
    let Some(parent) = caps.first() else {
        return Err(AsbErr::Exit(2));
    };
    let secret = match cli.secret_source()? {
        Some(src) => Some(load_secret(&src)?),
        None => None,
    };
    do_attenuate(secret.as_deref(), parent, &child_grant)
}

fn cmd_serve(cli: &Cli) -> Result<(), AsbErr> {
    let ep = cli.at(1, "endpoint")?;
    let log = cli
        .pos
        .get(2)
        .cloned()
        .unwrap_or_else(|| format!("{ep}.log"));
    cli.no_more_than(3)?;
    cli.only(
        "serve",
        &[
            "--tcp",
            "--handshake",
            "--key-env",
            "--key-file",
            "--secret-env",
            "--secret-file",
            "--revocation-file",
            "--unix",
            "--allow-remote",
            "--durability",
            "--horizon-ms",
            "--health-ms",
            "--max-retained-records",
            "--max-retained-bytes",
            "--retain-file",
            "--retention-interval-ms",
        ],
    )?;
    let health_every = match cli.num("--health-ms")? {
        None => SERVE_HEALTH_EVERY,
        Some(0) => {
            return Err(AsbErr::Exit(usage_error(
                "--health-ms 0: the health sidecar's period is at least 1 ms",
            )))
        }
        Some(ms) => Duration::from_millis(ms),
    };
    let max_retained_records = cli.num("--max-retained-records")?;
    let max_retained_bytes = cli.num("--max-retained-bytes")?;
    let retention_interval_ms = cli.num("--retention-interval-ms")?;
    let retain_file = cli.kv.get("--retain-file").cloned();
    if (max_retained_records.is_some()
        || max_retained_bytes.is_some()
        || retention_interval_ms.is_some()
        || retain_file.is_some())
        && !cfg!(feature = "retention")
    {
        return Err(AsbErr::Exit(usage_error(
            "serve --retain-file/--retention-interval-ms/--max-retained-records/--max-retained-bytes requires asb built with the `retention` feature",
        )));
    }
    if retention_interval_ms == Some(0) {
        return Err(AsbErr::Exit(usage_error(
            "--retention-interval-ms expects at least 1",
        )));
    }
    #[cfg(feature = "retention")]
    let retention = match &retain_file {
        Some(path) => match read_retain_file(path) {
            Ok(r) => Some(r),
            Err(e) => return Err(AsbErr::Exit(usage_error(&e))),
        },
        None => None,
    };
    // Every refusal below is decided BEFORE a secret is read or the log is
    // opened, so a usage error consumes no `--*-env` variable and leaves no
    // file behind.
    let tcp = cli.has("--tcp") || cli.key_source()?.is_some();
    let beside = cli.kv.get("--unix").cloned();
    if !tcp {
        if beside.is_some() {
            return Err(AsbErr::Exit(usage_error(
                "--unix needs a TCP listener (--tcp, or a key): without one the endpoint IS the Unix socket",
            )));
        }
        if cli.has("--allow-remote") {
            return Err(AsbErr::Exit(usage_error(
                "--allow-remote applies to a TCP listener; a Unix socket is never remote",
            )));
        }
    }
    let guard = cli.secret_source()?;
    if guard.is_some() && !cfg!(feature = "cap") {
        return Err(AsbErr::Exit(usage_error(
            "serve --secret-env/--secret-file (a GUARDED broker) requires asb built with `--features cap`",
        )));
    }
    let durability = serve_durability(cli)?;
    if tcp {
        match is_loopback_endpoint(&ep) {
            Ok(true) => {}
            Ok(false) if cli.has("--allow-remote") => {}
            Ok(false) => {
                return Err(AsbErr::Exit(usage_error(&format!(
                    "{ep} is not a loopback address, and a broker bound there is reachable from the network: plain --tcp has no boundary at all, and a pre-shared key is one secret every host of the fleet holds — a transport boundary, not a per-host identity (only a --secret-file guard says which node may publish what). Pass --allow-remote to say that is what you mean"
                ))));
            }
            Err(e) => {
                return Err(AsbErr::Exit(usage_error(&format!(
                    "{ep}: {e} (a <host>:<port> to bind)"
                ))));
            }
        }
    }
    // The key this broker serves under is read HERE, once, owner-only, where it was
    // only checked before: the mode is read off the descriptor the key bytes come
    // from (`KeyFile::OwnerOnly`), never off the path and then the path read again.
    let transport = cli.transport_reading(KeyFile::OwnerOnly)?;
    let secret = match &guard {
        Some(src) => Some(load_secret(src)?),
        None => None,
    };
    let guarded = secret.is_some();
    if matches!(transport, Transport::Unix) {
        refuse_live_socket(&ep)?;
    }
    if let Some(sock) = &beside {
        refuse_live_socket(sock)?;
    }
    let created_here = !std::path::Path::new(&log).exists();
    let broker = open_log(&log, secret, durability)?;
    #[cfg(feature = "retention")]
    {
        if let Some(max_rec) = max_retained_records {
            broker.set_max_retained_records(usize::try_from(max_rec).unwrap_or(usize::MAX));
        }
        if let Some(max_bytes) = max_retained_bytes {
            broker.set_max_retained_bytes(usize::try_from(max_bytes).unwrap_or(usize::MAX));
        }
        if let Some(ms) = retention_interval_ms {
            broker.set_retention_interval(std::time::Duration::from_millis(ms));
        }
        if let Some(rules) = retention {
            if let Err(e) = broker.set_retention(rules) {
                let path = retain_file.as_deref().unwrap_or_default();
                discard_created_log(&log, created_here, broker);
                return Err(AsbErr::Exit(fail_exit(
                    2,
                    &format!("--retain-file {path}: {e}"),
                )));
            }
        }
    }
    if let Some(rev_path) = cli.kv.get("--revocation-file") {
        if let Err(e) = broker.load_revocation_file(rev_path) {
            discard_created_log(&log, created_here, broker);
            return Err(AsbErr::Exit(fail_exit(
                2,
                &format!("--revocation-file {rev_path}: {e}"),
            )));
        }
    }
    let served = match transport {
        Transport::Sealed(key) => serve_sealed(&broker, &ep, key.as_ref())?,
        Transport::Handshake(key) => serve_handshake(&broker, &ep, key.as_ref())?,
        Transport::Tcp => broker.serve_tcp(&ep),
        Transport::Unix => broker.serve(&ep),
    };
    let handle = match served {
        Ok(h) => h,
        Err(e) => {
            return Err(AsbErr::Exit(refuse_serve(
                &log,
                created_here,
                &ep,
                &e,
                broker,
            )))
        }
    };
    // THE SOCKET BESIDE THE PORT: one broker, one log, one guard — two
    // acceptors.
    let beside_handle = match beside.as_deref().map(|sock| (sock, broker.serve(sock))) {
        None => None,
        Some((_, Ok(h))) => Some(h),
        Some((sock, Err(e))) => {
            drop(handle);
            return Err(AsbErr::Exit(refuse_serve(
                &log,
                created_here,
                sock,
                &e,
                broker,
            )));
        }
    };
    let bound = handle.tcp_addr().map_or(ep, str::to_string);
    // THE HEALTH SIDECAR, `<log>.health`, once every listener is bound (a refused bind
    // leaves none behind): written now, every period, at the poison, and a last time
    // at a clean stop — what `asb stats` reads while this broker holds the log. Not
    // fatal: a log directory that refuses it still serves, and says so.
    if let Err(e) = broker.keep_health(health_every) {
        warn(&format!(
            "asb: {log}{}: cannot write the health sidecar ({e}); `asb stats` cannot read \
             this broker until it can",
            astream_broker::HEALTH_SUFFIX
        ));
    }
    // The readiness line — callers sync on this, not a sleep — so a caller
    // that cannot read it will hang waiting for it. `say` flushes and reports
    // the write error instead of parking forever behind a dead reader. With
    // --unix a second line follows once that socket is bound too.
    say(&format!("listening {bound}"))?;
    if let Some(sock) = &beside {
        say(&format!("listening {sock}"))?;
    }
    warn(&format!(
        "asb: serving {bound}{} — {}",
        beside
            .as_deref()
            .map_or_else(String::new, |sock| format!(" and {sock}")),
        if guarded {
            "GUARDED: an attach must carry a capability minted under the secret"
        } else {
            "unguarded: no capability is checked on attach"
        }
    ));
    // The tier IN FORCE, as the broker decided it (and as `Hello` tells every client):
    // never inferred from the flag, which an Ordered request served Strict would
    // misreport.
    let served = broker.served_durability();
    warn(&format!(
        "asb: durability {} — {}",
        tier_word(served.requested),
        if served.served == served.requested {
            format!("served as asked{}", horizon_note(&served))
        } else {
            format!(
                "served {}: {:?}, never weaker than asked",
                tier_word(served.served),
                served.reason
            )
        }
    ));
    // Dropping a handle shuts its acceptor down and unlinks its socket.
    let _keep = (handle, beside_handle);
    let mut parks = 0u64;
    while parks < u64::MAX {
        std::thread::park();
        parks = parks.saturating_add(1);
    }
    Ok(())
}

fn cmd_pub(cli: &Cli) -> Result<(), AsbErr> {
    let ep = cli.at(1, "endpoint")?;
    let subject = cli.at(2, "subject")?;
    cli.no_more_than(3)?;
    cli.only_net("pub", &["--id", "--seq", "--seq-file"])?;
    let seq_file = cli.kv.get("--seq-file").cloned();
    if seq_file.is_some() && cli.kv.contains_key("--seq") {
        return Err(AsbErr::Exit(usage_error(
            "--seq and --seq-file both set the producer sequence; pass one",
        )));
    }
    let caps = cli.caps()?;
    // Parse --seq HERE, not after the connect: a malformed value is a usage
    // error (exit 2) even when the endpoint is not listening.
    let explicit_seq = cli.num("--seq")?;
    // Default to a UNIQUE (id, seq) per invocation, not fixed constants:
    // the broker dedups by (producer_id, producer_seq), so two independent
    // publishers both defaulting to (1, 0) would collide and the second
    // publish would be silently dropped as a dup. A deliberate idempotent
    // retry passes an explicit sequence, and a sequence is only a dedup key
    // under an id that survives the process: a per-invocation id would make
    // a retried `--seq` a fresh record, so either sequence flag demands one.
    let stable_for = if seq_file.is_some() {
        Some("--seq-file")
    } else if explicit_seq.is_some() {
        Some("--seq")
    } else {
        None
    };
    let id = producer_id(cli, &caps, stable_for)?;
    // The top half of the sequence space belongs to `ack` (see ACK_SEQ_BASE),
    // so a publish may not reach into it and collide with one.
    if explicit_seq.is_some_and(|seq| seq >= ACK_SEQ_BASE) {
        return Err(AsbErr::Exit(usage_error(
            "--seq must be below 2^63: sequences at or above it are reserved for `asb ack`, which publishes under 2^63|<input offset> so that an ack and an ordinary publish under the same producer id can never share a dedup key",
        )));
    }
    let mut body = Vec::new();
    io::stdin().read_to_end(&mut body)?;
    // A body over the record cap can never be stored, so retrying cannot
    // help: refuse it as bad input before connecting or burning a sequence.
    if body.len() > MAX_RECORD_PAYLOAD {
        return Err(AsbErr::Exit(usage_error(&format!(
            "the body is {} bytes; a record holds at most {MAX_RECORD_PAYLOAD}",
            body.len()
        ))));
    }
    let (c, _) = connect_attached(cli, &ep, &caps)?;
    // The write-ahead lands here: after the connection is up (so an endpoint
    // that is not there costs no sequence number) and before the Publish.
    let seq = match &seq_file {
        Some(path) => advance_seq_file(path)?,
        None => explicit_seq.unwrap_or_else(now_nanos),
    };
    Ok(do_pub(c, id, seq, &subject, &body)?)
}

fn cmd_sub(cli: &Cli) -> Result<(), AsbErr> {
    let ep = cli.at(1, "endpoint")?;
    let filter = cli.at(2, "filter")?;
    cli.no_more_than(3)?;
    cli.only_net("sub", &["--from", "--group"])?;
    let from = cli.num("--from")?;
    let group = cli.kv.get("--group").cloned();
    if group.is_some() && from.is_some() {
        return Err(AsbErr::Exit(usage_error(
            "--group resumes from the group's committed offset; it cannot be combined with --from",
        )));
    }
    let caps = cli.caps()?;
    let (c, _) = connect_attached(cli, &ep, &caps)?;
    let sub = match group {
        Some(g) => c.subscribe_group(&g, &filter)?,
        None => c.subscribe(from.unwrap_or(0), &filter)?,
    };
    Ok(do_sub(sub)?)
}

// RETAINED STATE: the last record of every subject the filter matches.
fn cmd_last(cli: &Cli) -> Result<(), AsbErr> {
    let ep = cli.at(1, "endpoint")?;
    let filter = cli.at(2, "filter")?;
    cli.no_more_than(3)?;
    cli.only_net("last", &["--after", "--max"])?;
    let after = match cli.kv.get("--after") {
        Some(s) => s.as_str(),
        None => "",
    };
    let max = cli.num32("--max")?.unwrap_or(DEFAULT_PAGE);
    let caps = cli.caps()?;
    let (mut c, _) = connect_attached(cli, &ep, &caps)?;
    // `last_all`, NOT `Client::last`: the latter drops the resume cursor, and a
    // page the broker's own row/scan bounds cut short would be printed as if it
    // were the whole answer. What that does NOT buy is one snapshot — see
    // `last_all`: the MARK is the FIRST request's, and a subject superseded past
    // a request's pinned head is omitted from its page.
    let (records, next, head) = last_all(&mut c, &filter, after, max)?;
    Ok(print_page(&records, next, head)?)
}

// A BOUNDED READ that does not tail: page through history with --from.
fn cmd_fetch(cli: &Cli) -> Result<(), AsbErr> {
    let ep = cli.at(1, "endpoint")?;
    let filter = cli.at(2, "filter")?;
    cli.no_more_than(3)?;
    cli.only_net("fetch", &["--from", "--max"])?;
    let from = cli.num("--from")?.unwrap_or(0);
    let max = cli.num32("--max")?.unwrap_or(DEFAULT_PAGE);
    let caps = cli.caps()?;
    let (mut c, _) = connect_attached(cli, &ep, &caps)?;
    let (records, (next, head)) = c.fetch(from, &filter, max)?;
    Ok(print_page(&records, next, head)?)
}

// A BATCH from a durable consumer group, then its commit — the shell face of
// `client::drain`. Two connections, because a group subscription's own
// connection is a one-way delivery stream.
fn cmd_drain(cli: &Cli) -> Result<(), AsbErr> {
    let ep = cli.at(1, "endpoint")?;
    let group = cli.at(2, "group")?;
    let filter = cli.at(3, "filter")?;
    cli.no_more_than(4)?;
    cli.only_net("drain", &["--max", "--idle", "--peek"])?;
    let max = cli.num("--max")?.unwrap_or(u64::from(DEFAULT_PAGE)) as usize;
    let idle_ms = cli.num("--idle")?.unwrap_or(DEFAULT_IDLE_MS);
    // Range-checked HERE, before anything is connected:
    // `set_read_timeout(Some(Duration::ZERO))` is `InvalidInput` by std's
    // contract, and would otherwise fail only after the group subscription is
    // registered, as an exit 1 naming neither the flag nor the verb.
    if idle_ms == 0 {
        return Err(AsbErr::Exit(usage_error(
            "--idle expects a positive number of milliseconds: 0 is not a non-blocking take, it is a read timeout the OS refuses",
        )));
    }
    let idle = Duration::from_millis(idle_ms);
    let peek = cli.has("--peek");
    let caps = cli.caps()?;
    let (c, closer) = connect_attached(cli, &ep, &caps)?;
    let mut committer = if peek {
        None
    } else {
        let (mut c, _) = connect_attached(cli, &ep, &caps)?;
        // The committer idles through the whole take, and the broker reaps a
        // connection that has sent no request within its first-frame timeout:
        // one round trip now keeps it however long the take runs. (A guarded
        // broker reaps one with no capability accepted by then; the attach
        // above is what keeps it there.)
        c.hello()?;
        Some(c)
    };
    let mut sub = c.subscribe_group(&group, &filter)?;
    // Bound the wait AFTER the attach round trip, so a short --idle can never
    // time the capability handshake out.
    closer.set_read_timeout(Some(idle))?;
    let records = match committer.as_mut() {
        // The commit lands BEFORE the print. HONEST BOUNDARY: a crash between
        // them loses that batch (the group has moved past it); `--peek` plus an
        // explicit `asb commit` is the at-least-once shape for a sink that
        // cannot afford that.
        Some(committer) => astream_broker::drain(&mut sub, committer, &group, max)?,
        None => astream_broker::take(&mut sub, max)?,
    };
    if let Some(gap) = sub.take_gap() {
        warn_gap("drain", &gap);
    }
    let mut out = stdout_buffered();
    print_records(&mut out, &records)?;
    let upto = records
        .last()
        .map_or("-".to_string(), |(offset, _, _)| offset.to_string());
    writeln!(
        out,
        "DRAIN n={} upto={upto} committed={}",
        records.len(),
        if committer.is_some() && !records.is_empty() {
            "yes"
        } else {
            "no"
        }
    )?;
    Ok(out.flush()?)
}

// THE READ-PROCESS-WRITE ACK: the answer record and the cursor advance in
// ONE durable append, deduped by the input offset.
fn cmd_ack(cli: &Cli) -> Result<(), AsbErr> {
    let ep = cli.at(1, "endpoint")?;
    let group = cli.at(2, "group")?;
    let offset = cli.pos_num(3, "offset")?;
    let subject = cli.at(4, "subject")?;
    let verdict = cli.at(5, "verdict")?;
    cli.no_more_than(6)?;
    cli.only_net("ack", &["--id"])?;
    let Ok(verdict) = astream_wire::ReceiptVerdict::parse(&verdict) else {
        return Err(AsbErr::Exit(usage_error(&format!(
            "<verdict> is handled|refused|deferred, got {verdict:?}"
        ))));
    };
    let caps = cli.caps()?;
    // `ack`'s dedup key is (producer_id, 2^63 | input offset). A per-invocation
    // producer id would make every retry a fresh record — the exact property
    // this verb exists to provide — so a stable id is required, not defaulted.
    let id = producer_id(cli, &caps, Some("ack"))?;
    if offset >= ACK_SEQ_BASE {
        return Err(AsbErr::Exit(usage_error(
            "<offset> must be below 2^63: `ack` publishes under producer sequence 2^63|<input offset>, and a larger offset has no sequence left to name",
        )));
    }
    // The body is DESIGN-aterm-fabric.md §4.2's `in/…` ack, written by the codec
    // astream reads receipts with (`astream_wire::V1Receipt`, kind `ack` from the
    // subject): `v=1 t=<ms> re=<offset> verdict=<word>`, the word on `verdict=`,
    // the one key an aterm asker decodes. (It rode `state=`, which no reader
    // decoded, so a refusal arrived as an ack with no word.)
    let body = astream_wire::V1Receipt::Ack {
        re: astream_wire::Offset(offset),
        verdict: Some(verdict),
        sender_t_ms: Some(now_millis()),
    }
    .encode();
    let (mut c, _) = connect_attached(cli, &ep, &caps)?;
    // `<offset>` names a record the consumer acted on, so it must name a record
    // that EXISTS. The read-process-write advances the group past it in the same
    // durable append, and that cursor is monotone — an out-of-range ack skipped
    // the whole inbox permanently at exit 0. The probe reads under the OUTPUT
    // SUBJECT, which is authorized by the same grant this publish is (see
    // `visible_head`). A subject that is not even a valid filter is one the
    // broker will refuse as an output subject a moment later, so skip the probe
    // and let that refusal — exit 1, naming the subject — stand as it always has.
    if astream_wire::Filter::new(subject.as_str()).is_ok() {
        let head = visible_head(&mut c, &subject)?;
        refuse_past_head("offset", offset, head)?;
    }
    // The `producer_seq` is `ACK_SEQ_BASE | offset` — the SAME derivation, from
    // the SAME constant, as `astream_broker::ack`, so a retry that crosses the
    // two faces dedups (`broker.ack-key-is-one-wire-contract`). Do not derive
    // this key a third way. The frame is built here rather than through the
    // helper only because the operand checks above belong to the argv face.
    let (at, dup) = c.process_and_produce(
        id,
        ACK_SEQ_BASE | offset,
        &subject,
        body.as_bytes(),
        &group,
        offset,
    )?;
    let (what, line) = if dup {
        (
            "this ack was already on the log (the cursor is already past it)",
            format!("{at} dup"),
        )
    } else {
        (
            "the ack record IS on the log and the group cursor HAS advanced",
            format!("{at} new"),
        )
    };
    Ok(say_durable(&line, what)?)
}

fn cmd_commit(cli: &Cli) -> Result<(), AsbErr> {
    let ep = cli.at(1, "endpoint")?;
    let group = cli.at(2, "group")?;
    let upto = cli.pos_num(3, "upto")?;
    cli.no_more_than(4)?;
    cli.only_net("commit", &[])?;
    // The head probe reads under the GROUP NAME, so it is authorized by the
    // same grant the commit is (see `visible_head`). A group name that is not
    // even a valid FILTER cannot be probed — and cannot be committed under a
    // guarded broker either, since `grants_commit` needs it to parse as a
    // SUBJECT, and every valid subject is a valid filter. Refuse it here, where
    // the operator can still read why, rather than skip the range check.
    if let Err(e) = astream_wire::Filter::new(group.as_str()) {
        return Err(AsbErr::Exit(usage_error(&format!(
            "<group> {group:?} is not a valid subject ({e}): a consumer group's name is a subject the broker authorizes commits against"
        ))));
    }
    let caps = cli.caps()?;
    let (mut c, _) = connect_attached(cli, &ep, &caps)?;
    let head = visible_head(&mut c, &group)?;
    refuse_past_head("upto", upto, head)?;
    let offset = c.commit(&group, upto)?;
    Ok(say_durable(
        &format!("{offset} committed"),
        "the group cursor HAS advanced (a commit is monotone: nothing lowers it)",
    )?)
}

fn run() -> Result<(), AsbErr> {
    let cli = parse()?;
    match cli.pos.first().map(String::as_str) {
        Some("repair") => cmd_repair(&cli),
        Some("stats") => cmd_stats(&cli),
        Some("compact") => cmd_compact(&cli),
        Some("mint") => cmd_mint(&cli),
        Some("attenuate") => cmd_attenuate(&cli),
        Some("serve") => cmd_serve(&cli),
        Some("pub") => cmd_pub(&cli),
        Some("sub") => cmd_sub(&cli),
        Some("last") => cmd_last(&cli),
        Some("fetch") => cmd_fetch(&cli),
        Some("drain") => cmd_drain(&cli),
        Some("ack") => cmd_ack(&cli),
        Some("commit") => cmd_commit(&cli),
        Some(other) => Err(AsbErr::Exit(usage_error(&format!(
            "unknown verb {other:?}"
        )))),
        None => Err(AsbErr::Exit(usage_error("missing verb"))),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::from(0),
        Err(AsbErr::Exit(code)) => ExitCode::from(code),
        Err(AsbErr::Io(e)) => {
            warn(&format!("asb: {e}"));
            ExitCode::from(1)
        }
    }
}

/// The most a `--retain-file` may hold: far more than [`astream_broker::retention::MAX_RULES`]
/// rules take, and refused past it before it is read whole.
#[cfg(feature = "retention")]
const RETAIN_FILE_MAX: u64 = 64 << 10;

/// `serve --retain-file PATH`: the rules file, read and checked
/// ([`astream_broker::parse_rules`]) before the log is opened, so a bad file is a usage
/// error that touches nothing.
#[cfg(feature = "retention")]
fn read_retain_file(path: &str) -> Result<astream_broker::Retention, String> {
    use std::io::Read;
    let mut text = String::new();
    let read = std::fs::File::open(path).and_then(|f| {
        f.take(RETAIN_FILE_MAX.saturating_add(1))
            .read_to_string(&mut text)
    });
    match read {
        Ok(n) if n as u64 > RETAIN_FILE_MAX => Err(format!(
            "--retain-file {path}: longer than {RETAIN_FILE_MAX} bytes"
        )),
        Ok(_) => {
            astream_broker::parse_rules(&text).map_err(|e| format!("--retain-file {path}: {e}"))
        }
        Err(e) => Err(format!("--retain-file {path}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{advance_seq_file, decode_hex, print_records};

    /// Removes a test's scratch directory when dropped, so the test leaves nothing
    /// behind whether it passes or panics.
    struct Cleanup(std::path::PathBuf);

    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hex_decodes_exact_length_both_cases() {
        let mut out = [0u8; 4];
        decode_hex(b"00ffA5c3", &mut out).unwrap();
        assert_eq!(out, [0x00, 0xff, 0xa5, 0xc3]);
    }

    #[test]
    fn hex_rejects_wrong_length() {
        let mut out = [0u8; 32];
        assert!(decode_hex(&[b'a'; 63], &mut out).is_err());
        assert!(decode_hex(&[b'a'; 65], &mut out).is_err());
        assert!(decode_hex(&[b'a'; 64], &mut out).is_ok());
    }

    /// A 64-BYTE key containing a multibyte char is simply not hex — never a
    /// char-boundary panic.
    #[test]
    fn hex_multibyte_input_is_an_error_not_a_panic() {
        let mut s = String::from("aé"); // 'é' is 2 bytes → 3 bytes so far
        s.push_str(&"a".repeat(61)); // 64 bytes total, valid UTF-8
        assert_eq!(s.len(), 64);
        let mut out = [0u8; 32];
        let e = decode_hex(s.as_bytes(), &mut out).unwrap_err();
        assert!(e.contains("not valid hex"), "{e}");
    }

    /// `u8::from_str_radix` accepts a leading sign (`+f` → 0x0f); the strict
    /// nibble decoder does not.
    #[test]
    fn hex_rejects_signs_and_whitespace() {
        let mut out = [0u8; 1];
        assert!(decode_hex(b"+f", &mut out).is_err());
        assert!(decode_hex(b"-1", &mut out).is_err());
        assert!(decode_hex(b" f", &mut out).is_err());
        let mut out32 = [0u8; 32];
        assert!(decode_hex(&b"+f".repeat(32), &mut out32).is_err());
    }

    /// The persisted producer sequence rises from 1, survives the process, and
    /// refuses to guess at a file that does not hold a number — guessing would
    /// silently restart the sequence and let the broker dedup live records away as
    /// if they were retries.
    #[test]
    fn seq_file_advances_from_one_and_refuses_a_non_number() {
        let dir = std::env::temp_dir().join(format!("asbseq_{}", std::process::id()));
        let _tmp = Cleanup(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seq");
        let path = path.to_str().unwrap();
        let _ = std::fs::remove_file(path);

        assert_eq!(
            advance_seq_file(path).unwrap(),
            1,
            "a missing file starts at 0"
        );
        assert_eq!(advance_seq_file(path).unwrap(), 2);
        assert_eq!(std::fs::read_to_string(path).unwrap(), "2");
        // A fresh reader (the restart case) continues; it does not restart.
        assert_eq!(advance_seq_file(path).unwrap(), 3);

        std::fs::write(path, "not-a-number\n").unwrap();
        let e = advance_seq_file(path).unwrap_err();
        assert!(e.to_string().contains("is not an unsigned integer"), "{e}");
    }

    /// `serve --retain-file`: a rules file reads as its rules; a bad line is refused naming
    /// its line, and a file past `RETAIN_FILE_MAX` before it is read whole.
    #[cfg(feature = "retention")]
    #[test]
    fn a_retain_file_reads_as_its_rules_and_a_bad_one_is_refused() {
        let dir = std::env::temp_dir().join(format!("asbretain_{}", std::process::id()));
        let _tmp = Cleanup(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rules");
        let path = path.to_str().unwrap();
        std::fs::write(
            path,
            "# the fabric\nprefix age=7d bytes=256MiB /f/F/pub/*/*/ev\nhold last /f/F/pub/*/*/presence\n",
        )
        .unwrap();
        let r = super::read_retain_file(path).unwrap();
        assert_eq!(r.rules().len(), 2);
        assert_eq!(r.rules()[0].bytes(), Some(256 << 20));
        std::fs::write(path, "prefix /x/>\nhold size=1 /y\n").unwrap();
        let e = super::read_retain_file(path).unwrap_err();
        assert!(e.contains("line 2: "), "{e}");
        let long = "# padding\n".repeat(usize::try_from(super::RETAIN_FILE_MAX).unwrap() / 10 + 1);
        std::fs::write(path, long).unwrap();
        let e = super::read_retain_file(path).unwrap_err();
        assert!(e.contains("longer than"), "{e}");
        let e = super::read_retain_file(dir.join("absent").to_str().unwrap()).unwrap_err();
        assert!(e.starts_with("--retain-file "), "{e}");
    }

    /// An EMPTY or whitespace-only file is not a number either, and it is the shape
    /// a truncating wrapper (`: > n-a1.seq`), a torn `tee` or a crashed writer
    /// actually leaves behind. Reading it as 0 would restart the producer sequence
    /// at 1, and the broker would dedup every record until the counter passed the
    /// old high-water mark away as a retry — silently, at exit 0. Only an ABSENT
    /// file starts a sequence.
    #[test]
    fn seq_file_refuses_an_empty_or_whitespace_only_file() {
        let dir = std::env::temp_dir().join(format!("asbseqempty_{}", std::process::id()));
        let _tmp = Cleanup(dir.clone());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seq");
        let path = path.to_str().unwrap();

        for contents in ["", "   ", "\n", " \t\r\n "] {
            std::fs::write(path, contents).unwrap();
            let e = advance_seq_file(path)
                .map_err(|e| e.to_string())
                .expect_err("an empty seq file must not restart the sequence");
            assert!(
                e.contains("is not an unsigned integer"),
                "{contents:?}: {e}"
            );
            assert_eq!(
                std::fs::read_to_string(path).unwrap(),
                contents,
                "the refusal rewrote nothing"
            );
        }
        // ...while a file that is simply not there still starts at 1.
        std::fs::remove_file(path).unwrap();
        assert_eq!(advance_seq_file(path).unwrap(), 1);
    }

    /// The staging file is created fresh, never opened through whatever already sits
    /// at its name. In a shared directory anyone can plant `<path>.<pid>.new` as a
    /// symlink; opening it with a plain create truncated and overwrote the link's
    /// TARGET, and the rename then left the sequence file itself a symlink to it.
    #[cfg(unix)]
    #[test]
    fn seq_file_staging_never_writes_through_a_planted_symlink() {
        let dir = std::env::temp_dir().join(format!("asbseqlink_{}", std::process::id()));
        let _tmp = Cleanup(dir.clone());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seq");
        let path = path.to_str().unwrap();
        let victim = dir.join("victim");
        std::fs::write(&victim, "precious").unwrap();
        let staging = format!("{path}.{}.new", std::process::id());
        std::os::unix::fs::symlink(&victim, &staging).unwrap();

        assert_eq!(advance_seq_file(path).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "precious");
        let meta = std::fs::symlink_metadata(path).unwrap();
        assert!(
            meta.file_type().is_file(),
            "the sequence file is a plain file"
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "1");
    }

    /// The lock sidecar is persistent, so it cannot be created fresh each time — but
    /// it must not be CREATED through a symlink either: a dangling `<path>.lock`
    /// pointing anywhere made every invocation create an empty file at the link's
    /// target, under this process's identity.
    #[cfg(unix)]
    #[test]
    fn seq_file_lock_is_never_created_through_a_dangling_symlink() {
        let dir = std::env::temp_dir().join(format!("asbseqlock_{}", std::process::id()));
        let _tmp = Cleanup(dir.clone());
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seq");
        let path = path.to_str().unwrap();
        let target = dir.join("created-by-a-link");
        std::os::unix::fs::symlink(&target, format!("{path}.lock")).unwrap();

        let e = advance_seq_file(path).expect_err("a dangling lock link is refused");
        assert!(e.to_string().contains(".lock"), "{e}");
        assert!(!target.exists(), "nothing was created at the link's target");
        assert!(!std::path::Path::new(path).exists(), "nothing was advanced");

        // A lock file that is simply there — the ordinary case — is used as it is.
        std::fs::remove_file(format!("{path}.lock")).unwrap();
        assert_eq!(advance_seq_file(path).unwrap(), 1);
        assert_eq!(advance_seq_file(path).unwrap(), 2);
    }

    /// The wildcard-read header cannot be SPOOFED by a subject. `Subject::new`
    /// rejects control bytes but not 0x20, so a publisher holding a legitimate grant
    /// on a lane may publish to `/f/F/in/n-a1/evil 999`; in an
    /// `<offset> <subject> <nbytes>` order a consumer taking the third
    /// space-separated field as the byte count would read 999 and swallow the framing
    /// of every record after it. With both numbers first the parse is exact:
    /// `<offset>`, `<nbytes>`, then the subject as the REST of the line — which no
    /// subject can end early, because a subject may not contain a newline.
    #[test]
    fn the_record_header_cannot_be_spoofed_by_a_subject_holding_a_space() {
        let records: Vec<crate::Record> = vec![
            (7, "/f/F/in/n-a1/evil 999".to_string(), b"abc".to_vec()),
            (8, "/f/F/in/n-a1/next".to_string(), b"ZZ\n\0!".to_vec()),
        ];
        let mut out = Vec::new();
        print_records(&mut out, &records).unwrap();

        // Parse exactly as the module doc tells a bridge in another language to.
        let mut rest: &[u8] = &out;
        for (offset, subject, body) in &records {
            let nl = rest
                .iter()
                .position(|b| *b == b'\n')
                .expect("a header line");
            let header = std::str::from_utf8(&rest[..nl]).unwrap();
            let mut fields = header.splitn(3, ' ');
            let off: u64 = fields.next().unwrap().parse().expect("field 1 is <offset>");
            let n: usize = fields.next().unwrap().parse().expect("field 2 is <nbytes>");
            let subj = fields.next().expect("the subject is the rest of the line");
            assert_eq!(off, *offset);
            assert_eq!(n, body.len());
            assert_eq!(subj, subject, "the space in the subject is not a delimiter");
            rest = &rest[nl + 1..];
            assert_eq!(&rest[..n], &body[..], "exactly <nbytes> body bytes");
            assert_eq!(rest[n], b'\n', "then ONE newline");
            rest = &rest[n + 1..];
        }
        assert!(rest.is_empty(), "nothing after the last record");
    }
}
