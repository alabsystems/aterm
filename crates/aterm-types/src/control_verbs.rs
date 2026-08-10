// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control-protocol VERB TABLE: the single, typed source of truth for every
//! control-socket verb. One [`VerbSpec`] row per verb carries its op-class,
//! reply framing, targeting class, and help synopsis. Everything else projects
//! from here: the server maps `op` to its auth `Op` and generates its help
//! catalog from `help`; the aterm-ctl client parses replies by `framing`. So the
//! server (which produces a reply) and the client (which parses it) — plus the
//! catalog and the router — can never disagree. Lives in `aterm-types` because
//! both binaries depend on it (alongside the `control_socket` shared-protocol
//! module).

/// A verb's authority class. Neutral here so this crate needn't depend on the
/// server's `aterm-session`; the server maps it to its `Op`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpClass {
    /// Pure observer / view-state control (incl. `subscribe`, the push face of read).
    Read,
    /// Injects the human input vocabulary the driven program observes, plus the
    /// app-drive verbs (`turn`/`spawn`/`close`/`tab`/`invoke`).
    Write,
    /// The out-of-band `signal` class.
    Signal,
    /// Rewrites the DURABLE on-disk config (`settings set|unset` → `aterm.toml`).
    /// A strictly greater authority than `Write` — split out so a keystroke-
    /// injection edge cannot flip a default-OFF security knob. The server maps this
    /// to `Op::ConfigWrite`, which is NOT provisioned to child edges.
    ConfigWrite,
    /// Moves the selection out of the process onto the system clipboard (`copy`, the
    /// exfil boundary). Split out of `Read` because writing the OS pasteboard leaves
    /// the process. Maps to `Op::ClipboardWrite`, NOT provisioned to child edges.
    ClipboardWrite,
    /// Owner-only privilege verb or build/meta verb — no op-class; gated separately.
    Owner,
}

/// The connection-SCOPE gate a verb needs, ORTHOGONAL to its op-class. Most verbs
/// are `Scoped` (authority is exactly the op-class, checked per target); a few are
/// gated by connection scope BEFORE/around the op check. Making this a table field
/// (rather than a hardcoded dispatch branch) keeps the table the single source of
/// truth for these exceptions too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Access {
    /// Normal: authority is exactly `op`, checked per resolved target.
    Scoped,
    /// Non-sensitive build/meta provenance — answered for ANY authenticated scope
    /// BEFORE target resolution (`version`/`update`/`help`/`verbs`). Self-scoped, no
    /// session; a selector is meaningless.
    AnyScopeMeta,
    /// Owner-only: only the instance god-token may run it, regardless of op-class
    /// (`sessions`/`who`/`grant`/`revoke`/`whoami`/`dial-*`). A selector is rejected.
    OwnerOnly,
}

/// How a verb's reply is framed on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Framing {
    /// A single `OK …` / `ERR …` status line (the default).
    Status,
    /// `OK <n>` header then `n` content lines.
    Lines,
    /// `OK <nbytes>` header then an `nbytes` raw byte body.
    Bytes,
    /// A push stream: `OK subscribe <n>` + a `sub <local> <sid>` map, then frames.
    Push,
}

/// What an `@<sid>` selector means for a verb (the targeting rule).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    /// Acts on the resolved SESSION (text/turn/image/send/…).
    Session,
    /// APP-level: acts on the resolved instance's FRONT window; the selector routes
    /// to the instance (window/chrome/controls/open/settings/tab/invoke/spawn/…).
    App,
    /// Self-scoped / fleet-wide; a selector is rejected (sessions/who/grant/version/…).
    Meta,
}

/// One control verb, fully specified.
#[derive(Clone, Copy, Debug)]
pub struct VerbSpec {
    /// The verb keyword as typed on the socket (e.g. `text`, `turn`, `spawn`).
    pub name: &'static str,
    /// Authority class the verb needs (the server maps this to its `Op`).
    pub op: OpClass,
    /// How the verb's reply is framed on the wire.
    pub framing: Framing,
    /// What an `@<sid>` selector means for the verb.
    pub target: Target,
    /// The connection-scope gate (orthogonal to `op`): `Scoped` for the common case,
    /// `AnyScopeMeta` / `OwnerOnly` for the exceptions the dispatch used to hardcode.
    pub access: Access,
    /// One-line catalog synopsis (the server's help catalog is generated from these).
    pub help: &'static str,
}

use Access::{AnyScopeMeta, OwnerOnly, Scoped};
use Framing::{Bytes, Lines, Push, Status};
use OpClass::{ClipboardWrite, ConfigWrite, Owner, Read, Signal, Write};
use Target::{App, Meta, Session};

/// A `const`-friendly [`VerbSpec`] constructor for a normally-`Scoped` verb (keeps
/// the table terse; the vast majority of rows).
const fn v(
    name: &'static str,
    op: OpClass,
    framing: Framing,
    target: Target,
    help: &'static str,
) -> VerbSpec {
    VerbSpec {
        name,
        op,
        framing,
        target,
        access: Scoped,
        help,
    }
}

/// A [`VerbSpec`] constructor with an explicit non-`Scoped` [`Access`] gate — the
/// build/meta (`AnyScopeMeta`) and owner-only (`OwnerOnly`) exceptions declare their
/// scope gate IN the table here, so the dispatch reads it instead of hardcoding a
/// verb list.
const fn va(
    name: &'static str,
    op: OpClass,
    framing: Framing,
    target: Target,
    access: Access,
    help: &'static str,
) -> VerbSpec {
    VerbSpec {
        name,
        op,
        framing,
        target,
        access,
        help,
    }
}

/// THE TABLE. Ordered by function for a readable generated catalog.
pub const VERBS: &[VerbSpec] = &[
    // build & meta — answered for ANY authenticated scope BEFORE target resolution
    // (non-sensitive global provenance); the `AnyScopeMeta` access declares that.
    va(
        "version",
        Owner,
        Status,
        Meta,
        AnyScopeMeta,
        "build + compiler provenance (version, commit, rustc, flavor, signature)",
    ),
    va(
        "update",
        Owner,
        Status,
        Meta,
        AnyScopeMeta,
        "self-updater [status|check|apply]: staged build state; apply re-execs a staged build",
    ),
    va(
        "help",
        Owner,
        Lines,
        Meta,
        AnyScopeMeta,
        "this catalog (alias: verbs)",
    ),
    va(
        "verbs",
        Owner,
        Lines,
        Meta,
        AnyScopeMeta,
        "this catalog (alias: help)",
    ),
    // screen / terminal state
    v(
        "text",
        Read,
        Lines,
        Session,
        "the visible screen, one row per line",
    ),
    v(
        "screen",
        Read,
        Lines,
        Session,
        "the full styled grid as one lossless JSON frame",
    ),
    v(
        "line",
        Read,
        Status,
        Session,
        "line <n>: one physical line of scrollback+screen",
    ),
    v("lines", Read, Status, Session, "OK <scrollback-line-count>"),
    v(
        "cell",
        Read,
        Status,
        Session,
        "cell <r> <c>: OK <grapheme%enc> <fg> <bg> <attrs>[ link=]",
    ),
    v(
        "cursor",
        Read,
        Status,
        Session,
        "OK <row> <col> <visible> <style>",
    ),
    v(
        "dims",
        Read,
        Status,
        Session,
        "OK <rows> <cols> <px_w> <px_h>",
    ),
    v(
        "modes",
        Read,
        Lines,
        Session,
        "DEC modes: alt_screen=, cursor_visible=, ...",
    ),
    v(
        "title",
        Read,
        Status,
        Session,
        "OK <title> (from shell integration)",
    ),
    v(
        "cwd",
        Read,
        Status,
        Session,
        "OK <cwd> (from shell integration)",
    ),
    v("colors", Read, Status, Session, "OK fg= bg= cursor="),
    v(
        "search",
        Read,
        Lines,
        Session,
        "search <pat> [case] [regex]: full-history find, one \"<row> <col> <len>\" per match",
    ),
    v(
        "selection",
        Read,
        Lines,
        Session,
        "OK <n> + the selected text",
    ),
    // `copy` is the EXFIL BOUNDARY: it moves the selection OUT of the process onto
    // the system clipboard, so it is `ClipboardWrite`, not `Read`. `scroll`/`select`
    // (below) stay `Read` — viewport nav is part of reading and nothing leaves the
    // process. A read-only edge can pan and select but cannot exfiltrate.
    v(
        "copy",
        ClipboardWrite,
        Status,
        Session,
        "copy the selection to the system clipboard, OK <bytes>",
    ),
    v(
        "blocks",
        Read,
        Lines,
        Session,
        "shell-integration command blocks (exit codes, command text, state)",
    ),
    v(
        "blocktext",
        Read,
        Lines,
        Session,
        "blocktext <id>: one command block's output",
    ),
    // seeing: pixels, recording, history
    v(
        "image",
        Read,
        Status,
        Session,
        "image [path] : capture the APPLICATION-RENDERED CLIENT FRAME -> PNG, reply OK <w> <h> <path> (bare filename confined to the runtime images/ dir; auto-named when omitted). In a window the frame is bound to a successful application-present transaction; headless is a semantic-renderer artifact. Platform compositor visibility and scanout are not observed. image --bytes = return the PNG base64'd OVER THE WIRE (OK 1 + `<w> <h> <nbytes> <base64>`) instead of a server-local path — the form a REMOTE (dial/TLS) driver uses. image --meta = opt into an additive captured-frame pixel fingerprint plus terminal/native/composite phase, raster, paint, geometry, theme, and per-leaf metadata (existing replies stay byte-for-byte unchanged; with --bytes the reply is OK 2 + metadata and PNG rows). image plain = bare pixels; image read = inline OSC-1337 images as base64. @<sid> captures the app-render frame of the window showing that session",
    ),
    v(
        "window",
        Read,
        Status,
        App,
        "window [<target>] [path] : assemble a full-window artifact (platform-owned chrome + the exact submitted client destination) -> PNG, reply OK <w> <h> <path> (target: front|prefs|about|menu|update, default front; bare filename confined to images/). Compositor visibility and scanout are not observed",
    ),
    v(
        "video",
        Read,
        Status,
        App,
        "record N seconds (0.5..=60) of the front window's WSI-SUBMITTED destination frames -> frame_NNNN.png + index.json (same-clock timestamps; compositor visibility and scanout are not observed). Flags: full | keys (owner-only keystroke log: hardware input, plus socket input driven through the ACTIVE-TAB verbs — a CROSS-SESSION `@<sid>` verb, which is what the `@self` selector expands to, egresses on the control thread and is NOT logged, so drive FLAGLESS when you need key->frame latency) | pace (keep redraws flowing) | fps=<n> (cap capture rate, 1..=120) | budget=<MiB> (frame-store RAM, 64..=4096, default 512). Every recording carries >=1 baseline keyframe; retention converges to 8 eligible completed recordings while preserving fresh/live handoffs. `video status` = one-line read of the in-flight recording (recording= mode= elapsed_ms= frames= resized=); `video stop` = finalize it now. `video frames [count=N]` = no capture; list the newest recording's N highest-delta (most-changed) frames as `frame n= delta= t_us= seq= <path>` rows, so an AI pulls just the eventful key frames instead of every PNG (default 8, max 64). index.json meta reports honest coverage: head_truncated/evicted_frames/ring_skipped/covered_us vs requested_ms. key->captured-frame latency = first recorded submitted destination containing the glyph minus inputs[].t_us; cadence gaps = frames[].t_us deltas vs ~16667",
    ),
    v(
        "chrome",
        Read,
        Lines,
        App,
        "the front window's native macOS UI (toolbar + menu bar)",
    ),
    v(
        "controls",
        Read,
        Lines,
        App,
        "GUI controls as text (front|prefs|about|menu|update)",
    ),
    v(
        "panes",
        Read,
        Lines,
        App,
        "the front window's ACTIVE-tab split-pane layout: `layout tab=<i> panes=<n> zoomed=<bool>` header, then one `pane session=<sid> rect=<row_off>,<col_off>,<rows>x<cols> focused=<bool>` row per visible pane (cell coords; 1-cell divider gaps between rects). @<sid> describes the window whose ACTIVE tab displays that session, and errors when none does (background tab?)",
    ),
    v(
        "inspect",
        Read,
        Lines,
        App,
        "versioned native tab-app semantics: inspect app/v1 tabs | inspect app/v1 view <view-id> <text|controls|tree|audit>",
    ),
    v(
        "cast",
        Read,
        Bytes,
        Session,
        "asciicast v2 recording (compact, sendable); `cast frames [count=N]` expands it to a keyframe flipbook",
    ),
    v(
        "temporal",
        Read,
        Bytes,
        Session,
        "the screen reconstructed at a past instant (needs temporal_recording=true)",
    ),
    v(
        "history",
        Read,
        Lines,
        Session,
        "history [<n>] [since=<id>]: the turn LEDGER - id/submitted/status/dur_ms/seq/hash/text per completed turn",
    ),
    // `meta` reads/writes the USER-settable session metadata. Base op-class Read
    // (the bare form is a pure metadata readout); the `meta set`/`meta unset`
    // sub-forms are WRITES and are escalated to WriteInput by the server's
    // argument-aware `escalated_op` seam (session-scoped state like `lease` —
    // NOT ConfigWrite: nothing durable on disk is rewritten).
    v(
        "meta",
        Read,
        Status,
        Session,
        "meta -> OK title= user_title= description= icon= cwd= state= (pct-encoded; '-' = unset). `meta set <title|description|icon> <text...>` / `meta unset <field>` set or clear the USER metadata (write-gated; user title outranks the OSC title in tab labels; caps: title 120B, description 1024B, icon 64B)",
    ),
    // `status` is the READ-ONLY Subject+Status record (RFC: Tab Subject &
    // Status §8) — what a session IS and what it is DOING, classified entirely
    // locally. Versioned because a later interpretation tier consumes the same
    // record; there is no write sub-form, so it takes no `escalated_op` entry.
    v(
        "status",
        Read,
        Status,
        Session,
        "status -> OK schema=1 sid= subject= subject_source=pin|osc|cwd|unavailable observed= phase=unknown|starting|idle|running|quiet|exited since_ms= outcome=none|success|failure|signal exit_code= signal= detail= confidence=exact|strong|heuristic|unknown reasons= conflict= revision= enabled= : the session's SUBJECT + classified STATUS (pct-encoded; '-' = unset). Read-only. `observed=false` means never classified, which is NOT `phase=unknown` (classified, no evidence); `subject_source=unavailable` means the terminal lock was contended, never a silent fall to a lower rung. Fields are ADDITIVE and never bump the schema, so reject an unknown schema MAJOR rather than best-effort parsing, and treat an unknown phase/outcome/reason token as unknown rather than an error. `enabled=false` means `tab_status` is off and every phase will read unknown",
    ),
    v(
        "timeline",
        Read,
        Lines,
        Session,
        "timeline [<n>] [since=<id>]: the session EVENT TIMELINE - one `event <id> t=<ms> kind=<k> ...` line per lifecycle event (spawned/state-change/title-change/cwd-change/meta-change), monotonic ids, drop-oldest ring",
    ),
    v(
        "metrics",
        Read,
        Status,
        App,
        "render/latency counters [reset|percentiles] - percentiles: p50/p95/p99 input->application-present-return / output->application-present-return / frame-render distributions; plain line carries max_frame_gap_ms= (worst successful-present-return gap since reset), rust_main_to_first_present_ms= plus startup_phase_schema=1/startup_phase_valid= and eight exclusive startup_*_ms phases (router, GUI prepare, winit dispatch, initial surface attach, successful-redraw wait/compose/surface/finalize); startup_attach_schema=1/startup_attach_valid= drills the attach parent into dispatch/prepare/window-create/window-setup/backend-finalize/chrome-geometry/surface-create/finish, and the line ends first_present_ms= (compatibility GUI main_entry->the same successful-present publication; dyld/compositor/scanout unobserved)",
    ),
    // drive input
    v(
        "turn",
        Write,
        Lines,
        Session,
        "turn [idle=<ms>] [timeout=<ms>] [submit=<key|none>] [settle=match:<re>] [submit_window=<ms>] [presses=<n>] [submit_verify=<auto|seq|block>] <text>: ONE HUMAN TURN - type <text>, verified submit (submit=none types WITHOUT submitting; default 'enter', and any key-verb name is a valid submit key), wait for the screen to settle (idle for idle=<ms>, cap timeout=<ms>), return the settled screen. Reply: verdict line (id=/submitted=<0|1>/status=settled|timeout/seq=/dur_ms=/hash=<FNV16 of settled screen>) then the rows. App-agnostic; humans can interject",
    ),
    v(
        "lease",
        Write,
        Status,
        Session,
        "lease [status] | lease acquire [ttl=<ms>] [holder=<name>] | lease release [holder=<name>] [force] : a COOPERATIVE drive lease for raw (non-turn) drivers — one holder at a time, TTL-expiring (default 30000ms, max 600000), surfaced in `who` as driving=lease:<holder>. acquire refuses a live `turn` or a DIFFERENT holder's live lease (steals a lapsed one; same holder renews); a live lease also blocks a `turn` from stomping it. ADVISORY for raw send/key/feed (use `turn` for HARD arbitration) — the coordination signal cooperating agents check before driving",
    ),
    v(
        "send",
        Write,
        Status,
        Session,
        "write text to the PTY; reply `OK seq=<n>` — the content baseline, so `await seq <n+1>` waits for the output this input causes (same seq= on key/ctrl/feed/mouse/paste)",
    ),
    v(
        "paste",
        Write,
        Status,
        Session,
        "bracketed-paste text to the PTY",
    ),
    v(
        "key",
        Write,
        Status,
        Session,
        "send a named key (enter/tab/up/...)",
    ),
    v("ctrl", Write, Status, Session, "send a control char"),
    v("feed", Write, Status, Session, "write raw bytes to the PTY"),
    v(
        "feed-bin",
        Write,
        Status,
        Session,
        "length-prefixed raw bytes to the PTY",
    ),
    v(
        "paste-bin",
        Write,
        Status,
        Session,
        "length-prefixed bytes to the PTY with PASTE semantics (bracketed-paste guards + control-byte sanitize + LF->CR); the binary twin of `paste`",
    ),
    v("mouse", Write, Status, Session, "inject a mouse event"),
    v(
        "resize",
        Write,
        Status,
        Session,
        "resize <r> <c>: resize the engine + PTY (grid first, window echoed to match). \
         `resize px <w> <h>` instead resizes the WINDOW in physical pixels and lets the \
         grid follow from the platform resize event — the same path an edge drag takes, \
         so it is the form that exercises the live-resize width throttle (the cell form \
         pre-applies the grid, so the window event never sees a column change). Drive \
         several back to back to reproduce a drag's event pressure; read the result with \
         `metrics` (`resize_present`) and `dims` (`layer_*`)",
    ),
    v("focus", Write, Status, Session, "send focus in/out"),
    v(
        "scroll",
        Read,
        Status,
        Session,
        "scroll the view (up|down|top|bottom|N)",
    ),
    v(
        "select",
        Read,
        Status,
        Session,
        "select a region / word <r> <c> / line <r> / clear",
    ),
    v(
        "signal",
        Signal,
        Status,
        Session,
        "signal <sig>: send a signal to the foreground process group",
    ),
    // app / GUI drive (selector routes to the instance's front window)
    v(
        "tab",
        Write,
        Status,
        App,
        "drive the front window's tabs (new|N|next|prev)",
    ),
    v(
        "open",
        Write,
        Status,
        App,
        "open a native tab app: `open app settings [/route]` or `open app markdown|editor <local-file-uri>`; compatibility aux targets remain available",
    ),
    v(
        "act",
        Write,
        Status,
        App,
        "dispatch an exact semantic native-app action: act app/v1 view <view-id> <ui-key> <action> [value]",
    ),
    // `settings set|unset` atomically REWRITES the durable on-disk `aterm.toml`
    // (flipping default-OFF security knobs), so it is `ConfigWrite` — a strictly
    // greater authority than the `Write` keystroke class, and NOT carried by a
    // child's inherited write edge.
    v(
        "settings",
        ConfigWrite,
        Status,
        App,
        "settings [open|close|toggle], or `settings set|unset <key> [value...]`",
    ),
    v(
        "invoke",
        Write,
        Status,
        App,
        "invoke <action>: fire a menu action by name (enabled-gated; names via `controls menu`)",
    ),
    // Runtime-only per-session visual toggle (nothing durable is written); the
    // observability face scripts/tests read (`status` = one Status line).
    v(
        "rain",
        Write,
        Status,
        App,
        "rain [status|on|off|toggle]: matrix rain for the focused window's front session \
         (status prints config_enabled= session_override= effective= engine= active= \
         scope=window|focused-pane focused= animating=, plus a live engine's \
         weather= density= tick= scanned= material= emitting= vis= drain= seq= streak= diag)",
    ),
    // Read-only observability for an effect that is otherwise audible-only: the
    // tone-of-typing mood steering the trail synth's melody. No write form —
    // the knob is durable config (`settings set tone_melody`).
    v(
        "tone",
        Read,
        Status,
        App,
        "tone [status]: tone-of-typing state for the focused window (prints \
         tone= effective= knob= sounds= volume= audio=live|inert active= \
         window_chars= inferences=; `effective` is what the synth is stamping, \
         `inferences` separates \"the model ran and said technical\" from \
         \"the model never ran\". The typed window's TEXT is never reported)",
    ),
    v(
        "hover",
        Write,
        Status,
        App,
        "toggle the drop-target highlight",
    ),
    // session lifecycle
    v(
        "spawn",
        Write,
        Status,
        App,
        "spawn [cwd=<path>]: mint a new tab session, reply OK <sid> - immediately addressable",
    ),
    v(
        "close",
        Write,
        Status,
        Session,
        "@<sid> close: retire that session (close its tab) - the death half of spawn",
    ),
    // waits
    v(
        "ready",
        Read,
        Status,
        Session,
        "block until the session is alive and idle",
    ),
    v(
        "await",
        Read,
        Status,
        Session,
        "await <idle <ms>|seq [<n>]|match <re>|block> [timeout=<ms>]: block until a predicate latches",
    ),
    v(
        "wait",
        Read,
        Status,
        Session,
        "block until the running command completes (OSC-133)",
    ),
    // streaming
    v(
        "subscribe",
        Read,
        Push,
        Session,
        "subscribe @<sel>[,...] <streams> [since=][every-frame]: push DELTA/EVENT/GAP/BYTES; streams=screen,cursor,cells,bytes,events,sessions, at least one of them (a modifier-only list is `ERR usage`); sessions = instance lifecycle (`EVENT * session-created/exited <sid>` for sibling spawns/exits, no `ls` polling) and is OWNER-ONLY because it reports the whole roster, not just your targets — a scoped edge asking for it gets `ERR denied`; add `timestamps` (alias `ts`) INSIDE <streams> (`cells,ts`; trailing is `ERR unknown subscribe arg`) to prefix frames with `T <local|*> <t_us>` lines (video's clock) so the stream is a timed frame source — at most one per channel per wake, tagged `<local>` for session frames and `*` for `sessions` events, so the second token is not always numeric",
    ),
    // sessions, presence & capability — the OwnerOnly access declares the owner
    // gate IN the table (the dispatch reads it, no hardcoded verb list). `who`
    // keeps its `Read` op-class (a fleet-wide presence readout) yet is Owner-gated:
    // op-class and scope-gate are orthogonal, which is exactly why `access` is a
    // separate field.
    va(
        "sessions",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "OK <n> + one line per session (local/sid/parent/state/title). Owner-only",
    ),
    va(
        "who",
        Read,
        Lines,
        Meta,
        OwnerOnly,
        "PRESENCE: per session driving=<turn-id|-> watchers=<n> turns=<n> - the hand + the eye. Owner-only",
    ),
    va(
        "whoami",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "OK <session> <nonce> <scope>",
    ),
    // `family` is `Read`/`Scoped`: the no-arg form walks the RESOLVED (already
    // gated) session. Its EXPLICIT-sid sub-form (`family <sid>`) additionally
    // demands Owner in the handler — a PER-ARGUMENT check the static table cannot
    // express (the authority depends on whether an arbitrary node is named), so it
    // stays a documented runtime check beyond the table, not an `access` value.
    v(
        "family",
        Read,
        Lines,
        Session,
        "the session's parent + direct children",
    ),
    v(
        "edges",
        Read,
        Lines,
        Session,
        "inbound capability edges (--json). alias: grants",
    ),
    v(
        "grants",
        Read,
        Lines,
        Session,
        "inbound capability edges (--json). alias: edges",
    ),
    va(
        "grant",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "mint a cross-session edge (Owner only)",
    ),
    va(
        "revoke",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "revoke a cross-session edge (Owner only)",
    ),
    va(
        "dial",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "dial <name>: relay this connection over TLS to the saved network-drive peer <name> - subsequent verbs run on the remote (owner-only; a pre-relay failure answers one ERR line, success sends no local reply)",
    ),
    va(
        "dial-list",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "list saved network-drive connections",
    ),
    va(
        "dial-token",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "token for a saved network-drive connection",
    ),
];

/// The spec for `verb`, or `None` for an unknown verb.
#[must_use]
pub fn spec(verb: &str) -> Option<&'static VerbSpec> {
    VERBS.iter().find(|s| s.name == verb)
}

/// Whether `verb` is OWNER-ONLY per the table — only the instance god-token may run
/// it, regardless of op-class. The dispatch reads THIS (not a hardcoded verb list),
/// so classifying a verb `OwnerOnly` in the table is what gates it. Unknown verbs
/// are not owner-only (they fall through to `ERR unknown verb`).
#[must_use]
pub fn is_owner_only(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::OwnerOnly))
}

/// Whether `verb` is non-sensitive build/meta provenance answered for ANY
/// authenticated scope BEFORE target resolution (`version`/`update`/`help`/`verbs`).
#[must_use]
pub fn is_any_scope_meta(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::AnyScopeMeta))
}

/// Trailer emitted after a complete guarded response. Its unpredictable nonce
/// is generated by the server only after receiving the request, so a client
/// cannot pipeline a valid acknowledgement before consuming the response.
pub const ARTIFACT_REPLY_CHALLENGE_PREFIX: &str = "ACK-CHALLENGE ";
/// Client echo sent only after consuming the complete response and challenge.
pub const ARTIFACT_REPLY_ACK_PREFIX: &str = "ACK ";

/// Validate the fixed-width lowercase/uppercase hexadecimal nonce grammar used
/// by guarded artifact acknowledgement frames.
#[must_use]
pub fn valid_artifact_ack_nonce(nonce: &str) -> bool {
    nonce.len() == 32 && nonce.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Whether a successful reply for this request may carry exact capture/video
/// retention. Kept beside [`framing_of`] so the shipping client and server-side
/// protocol documentation share one classification. An `ERR` never needs the
/// acknowledgement.
#[must_use]
#[cfg_attr(trust_verify, trust::skip)]
pub fn artifact_reply_requires_ack(verb: &str, request: &str) -> bool {
    let req_no_sel = request
        .strip_prefix('@')
        .and_then(|rest| rest.split_once(' ').map(|(_, tail)| tail))
        .unwrap_or(request);
    let sub = req_no_sel.split_whitespace().nth(1);
    match verb {
        // Only file framebuffer captures carry the exact-name guard. `image
        // read` is an in-memory inline-image projection; `--bytes` returns the
        // encoded PNG directly and writes no server-side artifact.
        "image" => {
            sub != Some("read")
                && !req_no_sel
                    .split_whitespace()
                    .any(|token| matches!(token, "--bytes" | "bytes"))
        }
        "window" => true,
        // Status/stop are ordinary in-memory control replies. A recording result
        // and `video frames` both advertise retained server-local paths.
        "video" => !matches!(sub, Some("status" | "stop")),
        _ => false,
    }
}

/// The reply framing of `verb` for the full `request` line. SELECTOR-AWARE: a
/// leading `@<selector>` is skipped, and the `image read` / `cast frames`
/// SUB-FORMS flip their base verb's framing to `Lines` (bare `image` rasterizes to
/// a status line; bare `cast` is a byte body). Unknown/plain verbs are `Status`.
#[must_use]
// Skip: iterator absent std bodies.
#[cfg_attr(trust_verify, trust::skip)]
pub fn framing_of(verb: &str, request: &str) -> Framing {
    let req_no_sel = request
        .strip_prefix('@')
        .and_then(|r| r.split_once(' ').map(|x| x.1))
        .unwrap_or(request);
    let sub = req_no_sel.split_whitespace().nth(1);
    if verb == "image" && sub == Some("read") {
        return Lines;
    }
    // `image --bytes` / `image bytes` returns the PNG base64'd on one line
    // (`OK 1\n<w> <h> <nbytes> <base64>`), so it is Lines-framed, not the Status
    // `OK <w> <h> <path>` of a file capture. The flag may sit anywhere in the tail.
    if verb == "image"
        && req_no_sel
            .split_whitespace()
            .any(|t| t == "--bytes" || t == "bytes")
    {
        return Lines;
    }
    if verb == "cast" && sub == Some("frames") {
        return Lines;
    }
    // `video frames [count=N]` lists the newest recording's top-delta frames as
    // `OK <n>\n` + n rows — Lines-framed, unlike the base `video <secs>` capture
    // (a single Status `OK …` dump line) and `video status`/`video stop`.
    if verb == "video" && sub == Some("frames") {
        return Lines;
    }
    // `temporal status` replies a single Status `OK enabled=… …` line, NOT the
    // Bytes-framed screen reconstruction the base `temporal [tick]` returns — so it
    // must NOT inherit temporal's Bytes framing (the client would read the `OK …`
    // line as a byte count and report a malformed header).
    if verb == "temporal" && sub == Some("status") {
        return Status;
    }
    // `--json`/`json` read mode: the server wraps the body with `json_ok` = a
    // uniform `OK 1\n<body>` (Lines framing) for EVERY json-capable read verb. Verbs
    // whose plain reply is Status-framed (`cursor`, `dims`, `metrics`) must therefore
    // switch to Lines under the flag or the client reads only the `OK 1` header and
    // silently drops the JSON body. Harmless for the already-Lines members
    // (text/screen/…).
    if JSON_CAPABLE_VERBS.contains(&verb)
        && req_no_sel
            .split_whitespace()
            .any(|t| t == "--json" || t == "json")
    {
        return Lines;
    }
    spec(verb).map_or(Status, |s| s.framing)
}

/// The verbs whose reply the server wraps in `json_ok` under `--json`/`json`, and
/// which therefore switch to [`Framing::Lines`] under the flag.
///
/// This list MIRRORS the server's `json_ok` call sites (`aterm-gui`'s
/// `cmd_*_json` helpers). It is the one framing input that is not derivable from
/// [`VERBS`], because json-capability is a property of the server's handler, not
/// of the verb row — so it is a hand-maintained duplicate, and duplicates drift.
/// `metrics` was missing here while `cmd_metrics_json` happily wrote Lines, so
/// `metrics --json` framed as Status and the client silently DROPPED the JSON
/// body. Named and exported so `aterm-gui`'s
/// `json_ok_sites_match_the_json_capable_verbs` test can bind the two ends
/// together; add a verb here in the same change that adds its `_json` handler.
pub const JSON_CAPABLE_VERBS: &[&str] = &[
    "text", "screen", "cursor", "dims", "blocks", "edges", "grants", "metrics",
];

/// The generated per-verb catalog lines (`<name padded>  <help>`), in table order.
/// The server's help catalog is its fixed header followed by these — so the
/// catalog cannot drift from the table (it IS the table).
pub fn catalog_lines() -> impl Iterator<Item = String> {
    VERBS.iter().map(|s| format!("{:<28} {}", s.name, s.help))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every json-capable verb must be a REAL table row, and a `Read` one — a
    /// typo or a removed verb would otherwise sit here silently doing nothing.
    #[test]
    fn json_capable_verbs_are_real_read_verbs() {
        assert!(!JSON_CAPABLE_VERBS.is_empty(), "non-vacuity");
        for v in JSON_CAPABLE_VERBS {
            let s = spec(v).unwrap_or_else(|| panic!("json-capable {v:?} is not in VERBS"));
            assert_eq!(s.op, Read, "json-capable {v:?} should be a Read verb");
        }
    }

    /// The regression: `metrics --json` framed as Status while the server wrote
    /// Lines, so the client consumed the `OK 1` header and dropped the JSON body.
    #[test]
    fn json_flag_switches_every_json_capable_verb_to_lines() {
        for v in JSON_CAPABLE_VERBS {
            for flag in ["--json", "json"] {
                assert_eq!(
                    framing_of(v, &format!("{v} {flag}")),
                    Lines,
                    "{v} {flag} must frame as Lines (the json_ok `OK 1\\n<body>` shape)"
                );
            }
        }
        // Negative control: WITHOUT the flag the verb keeps its table framing, so
        // the test above is not passing for a trivial reason.
        assert_eq!(framing_of("metrics", "metrics"), Status);
        assert_eq!(framing_of("cursor", "cursor"), Status);
    }

    #[test]
    fn table_has_no_duplicate_verbs() {
        let mut names: Vec<&str> = VERBS.iter().map(|s| s.name).collect();
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "duplicate verb in the table");
    }

    #[test]
    fn framing_base_and_sub_forms() {
        assert_eq!(framing_of("text", "text"), Lines);
        assert_eq!(framing_of("who", "who"), Lines);
        assert_eq!(framing_of("cast", "cast"), Bytes);
        assert_eq!(framing_of("subscribe", "subscribe @. screen"), Push);
        assert_eq!(framing_of("cursor", "cursor"), Status);
        assert_eq!(framing_of("bogus", "bogus"), Status);
        assert_eq!(framing_of("image", "image shot.png"), Status);
        assert_eq!(framing_of("image", "image --meta shot.png"), Status);
        assert_eq!(framing_of("image", "image --meta --bytes"), Lines);
        assert_eq!(framing_of("image", "@child image read"), Lines);
        assert_eq!(framing_of("cast", "@s-a cast frames"), Lines);
        // `video frames` lists rows (Lines); base `video`/`video status` stay Status.
        assert_eq!(framing_of("video", "video frames"), Lines);
        assert_eq!(framing_of("video", "@s-a video frames count=5"), Lines);
        assert_eq!(framing_of("video", "video 3 full"), Status);
        assert_eq!(framing_of("video", "video status"), Status);
        // `temporal status` is a Status line; base `temporal`/`temporal <tick>` stay Bytes.
        assert_eq!(framing_of("temporal", "temporal status"), Status);
        assert_eq!(framing_of("temporal", "temporal"), Bytes);
        assert_eq!(framing_of("temporal", "temporal 1200"), Bytes);
        assert_eq!(framing_of("temporal", "@s-a temporal status"), Status);
        // `--json` read mode replies Lines-framed (`OK 1\n<body>`) for every
        // json-capable verb; the Status-base ones (cursor/dims) must flip or the
        // client drops the JSON body. Both `--json` and bare `json` are accepted.
        assert_eq!(framing_of("cursor", "cursor --json"), Lines);
        assert_eq!(framing_of("dims", "dims json"), Lines);
        assert_eq!(framing_of("cursor", "@s-a cursor --json"), Lines);
        assert_eq!(framing_of("edges", "edges --json"), Lines);
        // Without the flag, cursor/dims keep their Status framing.
        assert_eq!(framing_of("dims", "dims"), Status);
        // Session metadata: `meta` (and its set/unset sub-forms) are single
        // status lines; `timeline` streams `OK <n>` + n event lines.
        assert_eq!(framing_of("meta", "meta"), Status);
        assert_eq!(framing_of("meta", "meta set title build agent"), Status);
        assert_eq!(framing_of("meta", "@s-a meta unset icon"), Status);
        assert_eq!(framing_of("timeline", "timeline"), Lines);
        assert_eq!(framing_of("timeline", "@s-a timeline 10 since=3"), Lines);
        // The Subject+Status record is ONE versioned status line, with no
        // sub-form to change that.
        assert_eq!(framing_of("status", "status"), Status);
        assert_eq!(framing_of("status", "@s-a status"), Status);
    }

    #[test]
    fn artifact_ack_classification_covers_every_guarded_subform() {
        for (verb, request) in [
            ("image", "image"),
            ("image", "image shot.png"),
            ("window", "window prefs shot.png"),
            ("video", "video 3 full"),
            ("video", "@s-a video frames count=4"),
        ] {
            assert!(
                artifact_reply_requires_ack(verb, request),
                "{request} must acknowledge exact artifact retention"
            );
        }
        assert!(!artifact_reply_requires_ack("video", "video status"));
        assert!(!artifact_reply_requires_ack("video", "video stop"));
        assert!(!artifact_reply_requires_ack("image", "@s-a image read"));
        assert!(!artifact_reply_requires_ack("image", "image --bytes"));
        assert!(!artifact_reply_requires_ack("text", "text"));
        assert!(valid_artifact_ack_nonce("00112233445566778899aabbccddeeff"));
        assert!(!valid_artifact_ack_nonce("artifact"));
    }

    #[test]
    fn every_verb_has_a_nonempty_help_line() {
        assert_eq!(catalog_lines().count(), VERBS.len());
        assert!(VERBS.iter().all(|s| !s.help.is_empty()));
        assert!(spec("image").unwrap().help.contains("image --meta"));
    }

    /// The `Access` exceptions are the single source of truth for the scope gate the
    /// dispatch used to hardcode. Pin BOTH sets exactly so the table↔dispatch binding
    /// is total: a verb cannot become owner-only / any-scope-meta without being
    /// classified here (and the dispatch reads these predicates, not a verb list).
    #[test]
    fn access_exceptions_are_exactly_the_declared_sets() {
        let owner_only: Vec<&str> = VERBS
            .iter()
            .filter(|s| matches!(s.access, Access::OwnerOnly))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            owner_only,
            [
                "sessions",
                "who",
                "whoami",
                "grant",
                "revoke",
                "dial",
                "dial-list",
                "dial-token",
            ],
            "the OwnerOnly set (dispatch gates exactly these on Owner scope)",
        );

        let any_meta: Vec<&str> = VERBS
            .iter()
            .filter(|s| matches!(s.access, Access::AnyScopeMeta))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            any_meta,
            ["version", "update", "help", "verbs"],
            "the AnyScopeMeta set (answered pre-scope for any authenticated caller)",
        );

        // The predicates project the same truth.
        for v in &owner_only {
            assert!(is_owner_only(v), "{v} is_owner_only");
            assert!(!is_any_scope_meta(v), "{v} not any-scope-meta");
        }
        for v in &any_meta {
            assert!(is_any_scope_meta(v), "{v} is_any_scope_meta");
            assert!(!is_owner_only(v), "{v} not owner-only");
        }
        // A normal `Scoped` verb is neither; unknown verbs are neither.
        assert!(!is_owner_only("text") && !is_any_scope_meta("text"));
        assert!(!is_owner_only("bogus") && !is_any_scope_meta("bogus"));
    }

    /// `copy` is the clipboard-exfil boundary and `settings` rewrites durable config
    /// — each split OUT of the coarse Read/Write class into its own op-class, so a
    /// read-only / keystroke-only edge can no longer reach them.
    #[test]
    fn copy_and_settings_have_their_own_op_class() {
        assert_eq!(spec("copy").unwrap().op, OpClass::ClipboardWrite);
        assert_eq!(spec("settings").unwrap().op, OpClass::ConfigWrite);
        // scroll/select stay Read — viewport nav is part of reading, nothing leaves.
        assert_eq!(spec("scroll").unwrap().op, OpClass::Read);
        assert_eq!(spec("select").unwrap().op, OpClass::Read);
    }
}
