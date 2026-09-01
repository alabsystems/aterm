// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The control-protocol VERB TABLE: the single, typed source of truth for every
//! control-socket verb. One [`VerbSpec`] row per verb carries its op-class,
//! reply framing, targeting class, and help text — a first-sentence `summary`
//! plus the `detail` that completes it. Everything else projects from here: the
//! server maps `op` to its auth `Op` and generates BOTH help catalogs from the
//! two fields (the short one from the summaries, the full one from
//! [`VerbSpec::help_line`]); the aterm-ctl client parses replies by `framing`.
//! So the server (which produces a reply) and the client (which parses it) —
//! plus both catalogs and the router — can never disagree. Lives in `aterm-types` because
//! both binaries depend on it (alongside the `control_socket` shared-protocol
//! module).

/// Maximum schema-1 operator proposal body accepted by both the control client
/// and server. Keeping this admission bound in the shared protocol crate avoids
/// streaming a body after the server has already rejected and closed the frame.
pub const MAX_OPERATOR_PROPOSAL_BYTES: usize = 64 * 1024;

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
    /// Non-sensitive build/meta provenance and instance posture — answered for ANY
    /// authenticated scope BEFORE target resolution
    /// (`version`/`update`/`help`/`verbs`/`privacy`). Self-scoped, no session; a
    /// selector is meaningless.
    AnyScopeMeta,
    /// Owner-only: only the instance god-token may run it, regardless of op-class
    /// (`sessions`/`who`/`grant`/`revoke`/`whoami`/`dial-*`). A selector is rejected.
    OwnerOnly,
    /// Bridge-only: ONLY the fabric bridge connection may run it — the pair of
    /// `socketpair` ends the instance keeps when it launches its `aterm-link serve`
    /// child, served with the bridge scope PRE-RESOLVED instead of an `AUTH` line.
    /// The pinned set is FOUR verbs: `deliver`, `hold`, `outbox` and `outbox sent`.
    ///
    /// Strictly narrower than [`Self::OwnerOnly`], and deliberately so: Owner scope is
    /// what every in-session client already holds (`aterm-ctl @self` is Owner), so a
    /// prompt-injected agent holding Owner must not be able to forge an attested human
    /// order into a sibling's inbox (`deliver`, which stamps `from=`/`trust=`), lift a
    /// fleet halt locally (`hold`), or read every session's outbound traffic
    /// (`outbox`). There is no token file to steal: the only way to be the bridge is to
    /// be the process the instance spawned. A selector is rejected — every one of them
    /// names its session as an argument, like `raise`.
    ///
    /// A `hold` set through this gate has NO OPERATOR UNDO. `aterm-gui`'s
    /// `fabric::apply_hold` has exactly two production callers — `hold` itself and the
    /// fail-closed `bridge_lost` — and no GUI, palette, key-binding or Owner path
    /// clears one. So a `reason=fabric-lost` hold stands until a bridge RECONNECTS and
    /// issues `hold off`; if none can (a deleted cap file, a `[fabric] command` that
    /// exits at startup), the only recovery is restarting the instance. DESIGN §11.2
    /// says "or a human lifts it at the GUI"; that path does not exist, and this row
    /// says so rather than repeating it.
    BridgeOnly,
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
    /// The FIRST SENTENCE of the verb's help, at most [`SUMMARY_MAX_CHARS`] chars:
    /// the whole of this verb's row in the SHORT catalog (a bare `help`) and the head
    /// of its full entry. Short because the catalog an agent reads to FIND a verb must
    /// stay under [`SHORT_CATALOG_MAX_BYTES`] — with one string per verb it cost the
    /// whole of the `image`/`video`/`metrics` detail to find `text`.
    pub summary: &'static str,
    /// The rest of the entry — everything the summary does not say — or `""`. Shown
    /// by `help <verb>` and in the full catalog only. [`VerbSpec::help_line`] re-joins
    /// it after the summary with one space, so a full row whose split was a pure cut
    /// is the one-string help it came from, unchanged.
    pub detail: &'static str,
}

/// The cap on a [`VerbSpec::summary`] as a literal: the `help`/`verbs` rows quote it
/// through `concat!`, so the number an agent reads in the catalog and the number the
/// summary-length test enforces are one token that cannot drift apart.
macro_rules! summary_max_chars {
    () => {
        96
    };
}
/// The cap on a [`VerbSpec::summary`]: one readable row after the
/// [`CATALOG_TEXT_COLUMN`] gutter on a 125-column line.
pub const SUMMARY_MAX_CHARS: usize = summary_max_chars!();
/// The budget for the short catalog's rows put together (every [`catalog_lines`] row,
/// `\n`-terminated): what discovering a verb costs, whatever the table grows to.
///
/// RAISED FROM 8192 when the fabric's bridge plane completed (`outbox` / `outbox
/// sent`, DESIGN-aterm-fabric.md §11.2 as A3 settles it). The catalog was 8 170 B
/// at that moment — twenty-two bytes of headroom — so the choice was between
/// raising the number and rewording unrelated verbs' prose to make room, and
/// rewording a verb's help to fit an unrelated verb is exactly the drift a
/// generated golden exists to catch. What the budget actually bounds is the
/// SHAPE — one row per verb, first sentence only, capped at
/// [`SUMMARY_MAX_CHARS`] — and that shape is unchanged. The earlier move from
/// the design's 4 KiB to 8 KiB is the same accounting
/// (`docs/AGENT-EXPERIENCE-2026-08-26.md`, S7).
pub const SHORT_CATALOG_MAX_BYTES: usize = 9216;
/// The column a catalog row's text starts in: a 28-wide name plus one space.
pub const CATALOG_TEXT_COLUMN: usize = 29;
/// The width a `help <verb>` entry is wrapped to.
pub const ENTRY_WRAP_COLUMNS: usize = 100;

impl VerbSpec {
    /// The verb's FULL help text: `summary`, then `detail` after one space when there
    /// is any. Where the split was a pure cut this is exactly the one-string help the
    /// two fields came from, so the full catalog reads as before the split (the golden
    /// test pins every row, the six reworded ones included).
    #[must_use]
    pub fn help_line(&self) -> String {
        if self.detail.is_empty() {
            self.summary.to_string()
        } else {
            format!("{} {}", self.summary, self.detail)
        }
    }

    /// The verb's full entry as `help <verb>` prints it: `<name padded> <text…>`, the
    /// text greedily word-wrapped at [`ENTRY_WRAP_COLUMNS`] with every continuation
    /// line indented to [`CATALOG_TEXT_COLUMN`], so a 900-char entry reads as one
    /// aligned paragraph instead of one line. Deterministic and lossless: it breaks
    /// only at single spaces (the table carries no run of spaces), so re-joining the
    /// lines' text with spaces reproduces [`Self::help_line`] exactly; a word longer
    /// than the text width stands alone on its own line, never split, never dropped.
    #[must_use]
    // Skip: iterator absent std bodies.
    #[cfg_attr(trust_verify, trust::skip)]
    pub fn entry_lines(&self) -> Vec<String> {
        let text_width = ENTRY_WRAP_COLUMNS - CATALOG_TEXT_COLUMN;
        let mut chunks: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut cur_chars = 0usize;
        for word in self.help_line().split(' ') {
            let w = word.chars().count();
            if !cur.is_empty() && cur_chars + 1 + w > text_width {
                chunks.push(std::mem::take(&mut cur));
                cur_chars = 0;
            }
            if !cur.is_empty() {
                cur.push(' ');
                cur_chars += 1;
            }
            cur.push_str(word);
            cur_chars += w;
        }
        chunks.push(cur);
        let gutter = " ".repeat(CATALOG_TEXT_COLUMN);
        chunks
            .iter()
            .enumerate()
            .map(|(i, text)| {
                if i == 0 {
                    catalog_row(self.name, text)
                } else {
                    format!("{gutter}{text}")
                }
            })
            .collect()
    }
}

use Access::{AnyScopeMeta, BridgeOnly, OwnerOnly, Scoped};
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
    summary: &'static str,
    detail: &'static str,
) -> VerbSpec {
    VerbSpec {
        name,
        op,
        framing,
        target,
        access: Scoped,
        summary,
        detail,
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
    summary: &'static str,
    detail: &'static str,
) -> VerbSpec {
    VerbSpec {
        name,
        op,
        framing,
        target,
        access,
        summary,
        detail,
    }
}

/// THE TABLE. Ordered by function for a readable generated catalog. Each row's
/// help is two literals: the `summary` (its first sentence, the short catalog's
/// whole row — at most [`SUMMARY_MAX_CHARS`]) and the `detail` that completes it
/// (`""` when the summary says it all); `help_line` joins them with one space.
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
        "",
    ),
    va(
        "update",
        Owner,
        Status,
        Meta,
        AnyScopeMeta,
        "self-updater [status|check|apply]: staged build state; apply lands a staged build in place",
        "The apply is the seamless in-session handoff: the running app hands every window, tab, split and live shell to the staged build (your shells keep running; automatic within ~2 min by default, or now via `apply`). `relaunch_ready=` on `status` is a historical key name: a strictly-newer build is staged and can be applied in place now.",
    ),
    va(
        "help",
        Owner,
        Lines,
        Meta,
        AnyScopeMeta,
        "help [<verb> | --full]: this catalog (alias: verbs).",
        concat!(
            "Bare = one summary row per verb (first sentence, <= ",
            summary_max_chars!(),
            " chars); help <verb> = that verb's full entry, wrapped; help --full = the \
             complete catalog with the protocol header"
        ),
    ),
    va(
        "verbs",
        Owner,
        Lines,
        Meta,
        AnyScopeMeta,
        "verbs [<verb> | --full]: this catalog (alias: help).",
        concat!(
            "Bare = one summary row per verb (first sentence, <= ",
            summary_max_chars!(),
            " chars); help <verb> = that verb's full entry, wrapped; help --full = the \
             complete catalog with the protocol header"
        ),
    ),
    // The macOS consent posture: instance-wide, self-scoped, and `AnyScopeMeta` for
    // the same reason `version` is — an agent that just took an EPERM has to be able
    // to ask a `--headless` instance what its permission state is. `Read`, not
    // `Owner`: every fact here is one `status` already exposes a session at a time.
    va(
        "privacy",
        Read,
        Lines,
        Meta,
        AnyScopeMeta,
        "privacy [--json]: the macOS consent posture behind EPERM",
        "Self-scoped and instance-wide: no `@<sel>` (a selector is rejected), and a `--headless` \
         instance answers it. `OK <n>` then n lines — `schema=1`, the platform, the code-signing \
         identity macOS keys a grant to (`bundle_id= signing= team= dr= grant_stable=`), \
         `full_disk_access=` with the probe that read it, the `covers=`/`uncovered=` service \
         split, one `folder` line, `prompt_possible=`, one `session` line per LIVE session (never \
         truncated, so `sessions_total=` always equals the number of `session` lines), then \
         `containment`, `warmup=`, `observer`, `remediate` and a closing `note`. Free text is \
         pct-encoded and `-` is unset. Reading this verb raises NO dialog: the probe reads state \
         that already exists, and no value here is inferred from another. Per-folder state is \
         `unknown` BY CONSTRUCTION — the only way to learn whether a folder is readable is to \
         read it, which is the very act that raises the prompt — so a `folder` value leaves \
         `unknown` only where aterm itself observed an access (its own EPERM, or a warm-up the \
         human asked for), never because Full Disk Access is granted. `unavailable` on an \
         `observer` row is a THIRD value, distinct from `off` and from `false`: the observer \
         could not be consulted, which is not the same as its having answered no. \
         `full_disk_access=granted` removes this class of interruption for the folders that \
         grant covers; which services it covers is not measured here, so `fda_scope=unknown` and \
         the `folder` rows stay `unknown`. Only a human can change any of this — aterm cannot \
         grant it, and neither can you.",
    ),
    // screen / terminal state
    v(
        "text",
        Read,
        Lines,
        Session,
        "text [--json] [trim]: the visible screen, one row per line",
        "trim drops the trailing all-blank rows: the header becomes `OK <n> trimmed=<k>` with n = \
         the rows actually sent (interior blanks stay, so row i is still screen row i); `--json` \
         adds \"trimmed\":k and keeps dims.rows = the grid. Off by default (scripts count rows). \
         Any other argument is `ERR usage: text [--json] [trim]` — nothing is silently ignored",
    ),
    v(
        "screen",
        Read,
        Lines,
        Session,
        "the full styled grid as one lossless JSON frame",
        "",
    ),
    v(
        "line",
        Read,
        Status,
        Session,
        "line <n>: one physical line of scrollback+screen",
        "",
    ),
    v(
        "lines",
        Read,
        Status,
        Session,
        "OK <scrollback-line-count>",
        "",
    ),
    v(
        "cell",
        Read,
        Status,
        Session,
        "cell <r> <c>: OK <grapheme%enc> <fg> <bg> <attrs>[ link=]",
        "",
    ),
    v(
        "cursor",
        Read,
        Status,
        Session,
        "OK <row> <col> <visible> <style>",
        "",
    ),
    v(
        "dims",
        Read,
        Status,
        Session,
        "OK <rows> <cols> <px_w> <px_h>",
        "",
    ),
    v(
        "modes",
        Read,
        Lines,
        Session,
        "DEC modes: alt_screen=, cursor_visible=, ...",
        "",
    ),
    v(
        "title",
        Read,
        Status,
        Session,
        "OK <title> (from shell integration)",
        "",
    ),
    v(
        "cwd",
        Read,
        Status,
        Session,
        "OK <cwd> (from shell integration)",
        "",
    ),
    v("colors", Read, Status, Session, "OK fg= bg= cursor=", ""),
    v(
        "search",
        Read,
        Lines,
        Session,
        "search <pat> [case] [regex]: full-history find, one \"<row> <col> <len>\" per match",
        "(a hit straddling a SOFT WRAP is one match whose col+len runs past the grid width \
         and continues at column 0 of the next row; regex ^ and $ bind to the reader's \
         LOGICAL line, so a continuation row has no ^ of its own)",
    ),
    // `find` DRIVES the find bar; `search` above ANSWERS the same question without
    // one. Both are `Read`, and the reason is the op-class boundary rather than the
    // absence of mutation: find mode exists to DIVERT keystrokes away from the PTY,
    // so not one byte of a typed query reaches the driven program. What it moves is
    // the viewport and the highlight — precisely what `scroll` and `select` move,
    // and they are `Read` for the same reason. It reports the match position, which
    // is what `search` (`Read`) already answers, so a read edge learns nothing here
    // it could not already ask for. Classifying it `Write` would have been the real
    // escalation: `Op::WriteInput` and `Op::ReadScreen` are independent (a `push`
    // connection carries write WITHOUT read), so a keystroke-only edge could have
    // typed a query and read match positions back off a screen it may not read.
    v(
        "find",
        Read,
        Status,
        App,
        "find [open|type|key|next|prev|case|regex|accept|cancel|status]: drive the FIND BAR",
        "- the surface `key ctrl+f` opens and no other verb could reach (`send` writes to the PTY, \
         which is exactly what find mode diverts keystrokes AWAY from). Forms: `find open` = the \
         Edit ▸ Find… path itself; `find type <text>` inserts at the caret; `find key <name>` runs \
         one field edit, `<name>` one of back, delete, left, right, word-left, word-right, home, \
         end, kill-start, kill-end, kill-word-back, kill-word-forward; `find next`/`find prev` \
         step matches (⌘S/⌘R, wrapping); `find case`/`find regex` TOGGLE the ⌥⌘C/⌥⌘R flags (read \
         the reply to learn the new value — there is no set form because the keystroke has none); \
         `find accept` = ⏎ (exit, stay on the match), `find cancel` = ⎋ (exit, restore the \
         pre-find viewport); `find status` mutates nothing. Every form answers the SAME line: `OK \
         open=0` when no find bar is up, else `OK open=1 query=<pct> case= regex= regex_error= \
         matches=<n> current=<i> row= col= len= truncated= stale=`. row/col/len are the CURRENT \
         match in \
         `search` coordinates (negative row = scrollback) and are `-` when there is no match — \
         never a position that does not exist. current/matches are the 1-based `i/n` the find bar \
         itself paints, so the wire and the glass always agree; `truncated=1` means the search \
         index capped the batch, so that pair counts within the cap rather than over the whole \
         history, and `stale=1` means the terminal has CHANGED since that pair was counted (the \
         find bar paints the same fact as a trailing `…`): output is not re-searched per PTY \
         batch, so the count is a past census and can be wrong in either direction until the next \
         edit or `find next`/`find prev`. A form that types or steps while the bar is CLOSED changes nothing and answers \
         `open=0`; it can never fall through to the PTY. FRONT window of the resolved instance, \
         like `hover`",
    ),
    v(
        "selection",
        Read,
        Lines,
        Session,
        "OK <n> + the selected text",
        "",
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
        "",
    ),
    v(
        "blocks",
        Read,
        Lines,
        Session,
        "shell-integration command blocks (exit codes, command text, state)",
        "",
    ),
    v(
        "blocktext",
        Read,
        Lines,
        Session,
        "blocktext <id> [trim]: one command block's output",
        "trim drops the trailing all-blank rows (`OK <n> trimmed=<k>`, n = the rows sent); any \
         other tail is `ERR usage: blocktext <id> [trim]`",
    ),
    // seeing: pixels, recording, history
    v(
        "image",
        Read,
        Status,
        Session,
        "image [path] : capture the APPLICATION-RENDERED CLIENT FRAME -> PNG, reply OK <w> <h> \
         <path>",
        "(bare filename confined to the runtime images/ dir; auto-named when omitted). In a \
         window the frame is bound to a successful application-present transaction; headless is a \
         semantic-renderer artifact. Platform compositor visibility and scanout are not observed. \
         image --bytes = return the PNG base64'd OVER THE WIRE (OK 1 + `<w> <h> <nbytes> \
         <base64>`) instead of a server-local path — the form a REMOTE (dial/TLS) driver uses. \
         image --meta = opt into an additive captured-frame pixel fingerprint plus \
         terminal/native/composite phase, raster, paint, geometry, theme, and per-leaf metadata \
         (existing replies stay byte-for-byte unchanged; with --bytes the reply is OK 2 + \
         metadata and PNG rows). image plain = bare pixels; image read = inline OSC-1337 images \
         as base64. @<sid> captures the app-render frame of the window showing that session",
    ),
    v(
        "window",
        Read,
        Status,
        App,
        "window [<target>] [path] : assemble a full-window artifact",
        "(platform-owned chrome + the exact submitted client destination) -> PNG, reply OK <w> \
         <h> <path> (target: front|prefs|about|menu|update, default front; bare filename confined \
         to images/). Compositor visibility and scanout are not observed",
    ),
    v(
        "video",
        Read,
        Status,
        App,
        "record N seconds (0.5..=60) of the front window's WSI-SUBMITTED destination frames",
        "-> frame_NNNN.png + index.json (same-clock timestamps; compositor visibility and scanout are not observed). Flags: full | keys (owner-only keystroke log: hardware input, plus socket input aimed at the tab ON SCREEN — `key`, `ctrl`, `send`, `feed`, `paste`, flagless OR an explicit `@<sid>` naming the front tab — each stamped on the frame clock. A verb aimed at a BACKGROUND session egresses on the control thread and CANNOT be logged (`@self` expands to that when the driving session is not front), and input that lands on a WINDOW this take is not capturing (the front window changed mid-take — an `aterm ctl spawn` alone does it) has no frame here that could answer it; those attempts are COUNTED instead and reported as `unlogged_inputs=` on the reply line, live as `unlogged=` on `video status`, and in index.json meta (with the window share broken out as `unlogged_other_window`), so an empty inputs[] is never ambiguous and a logged row is never a key the recorded window never saw. Drive the FRONT tab when you need key->frame latency) | pace (keep redraws flowing) | fps=<n> (cap capture rate, 1..=120) | budget=<MiB> (frame-store RAM, 64..=4096, default 512). Every recording carries >=1 baseline keyframe; retention converges to 8 eligible completed recordings while preserving fresh/live handoffs. `video status` = one-line read of the in-flight recording (recording= mode= elapsed_ms= frames= resized= keys=, and for a keys take the RUNNING inputs= unlogged= so a driver learns mid-take that it is driving an unloggable path); `video stop` = finalize it now. `video frames [count=N]` = no capture; list the newest recording's N highest-delta (most-changed) frames as `frame n= delta= t_us= seq= <path>` rows, so an AI pulls just the eventful key frames instead of every PNG (default 8, max 64). index.json meta reports honest coverage: head_truncated/evicted_frames/ring_skipped/covered_us vs requested_ms, plus keys_requested/inputs_logged/unlogged_inputs. key->captured-frame latency = first recorded submitted destination containing the glyph minus inputs[].t_us (an inputs[] row is `ch` for a character or `key` for a named key like ArrowUp/Escape — NOT key->photon and NOT comparable to a keystroke-sampled latency: the recorder can only see a key's effect on its NEXT captured frame, so at fps=N no reading goes below ~1000/N ms however fast the terminal is. index.json `analysis` publishes that floor with every number (per row capture_floor_ms/at_capture_floor; per take capture_floor_p50_ms, capture_interval_p50_ms, attempts_outpace_readings, capture_verdict) — use `metrics percentiles` input_p50/p95/p99_ms when you want typing latency); cadence gaps = frames[].t_us deltas vs ~16667",
    ),
    v(
        "appstatus",
        Read,
        Lines,
        App,
        "OK <n> + one `activity` row per live/finished app-initiated job",
        "What aterm has been doing on its own initiative — a toolchain install, a self-update download — as the two status bars show it, plus the finished jobs the ring still remembers. Rows are `activity kind=<toolchain|update> phase=<live|done> progress=<pct>/100 title=<t> detail=<d> stats=<s> outcome=<ok|warn|-> [since_ms=<ms>]`; live rows first, then finished oldest-first. Free text is percent-encoded. Read-only: it starts and cancels nothing.",
    ),
    v(
        "chrome",
        Read,
        Lines,
        App,
        "the front window's native macOS UI (toolbar + menu bar)",
        "",
    ),
    v(
        "controls",
        Read,
        Lines,
        App,
        "GUI controls as text (front|prefs|about|menu|update)",
        "",
    ),
    v(
        "panes",
        Read,
        Lines,
        App,
        "the front window's ACTIVE-tab split-pane layout: `layout tab=<i> panes=<n> zoomed=<bool>`",
        "header, then one `pane session=<sid> rect=<row_off>,<col_off>,<rows>x<cols> \
         focused=<bool>` row per visible pane (cell coords; 1-cell divider gaps between rects). \
         @<sid> describes the window whose ACTIVE tab displays that session, and errors when none \
         does (background tab?)",
    ),
    v(
        "inspect",
        Read,
        Lines,
        App,
        "versioned native tab-app semantics: inspect app/v1 tabs",
        "| inspect app/v1 view <view-id> <text|controls|tree|audit>",
    ),
    v(
        "cast",
        Read,
        Bytes,
        Session,
        "asciicast v2 recording (compact, sendable);",
        "`cast frames [count=N]` expands it to a keyframe flipbook",
    ),
    v(
        "temporal",
        Read,
        Bytes,
        Session,
        "temporal [status|<tick>] [trim]: the screen reconstructed at a past instant",
        "(needs temporal_recording=true; `temporal status` reports the reachable tick range). trim \
         drops the trailing all-blank rows — byte-framed, so `OK <nbytes> trimmed=<k>` counts the \
         trimmed body; `status` has no rows, so `status trim` is `ERR usage`",
    ),
    v(
        "history",
        Read,
        Lines,
        Session,
        "history [<n>] [since=<id>]: the turn LEDGER",
        "- id/submitted/status/dur_ms/seq/hash/text per completed turn",
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
        "meta -> OK title= user_title= description= icon= role= attention= cwd= state=",
        "(pct-encoded; '-' = unset). `meta set <title|description|icon|role|attention> <text...>` \
         / `meta unset <field>` set or clear the USER metadata (write-gated; user title outranks \
         the OSC title in tab labels; `role operator` names the fleet operator and a non-empty \
         `attention` is the typed needs-human escalation the menu-bar status item badges; caps: \
         title 120B, description 1024B, icon 64B, role 64B, attention 256B). No `window=` here, \
         for the reason `status` gives: `meta` is polled and the window lives on the main thread \
         — ask `sessions`/`ls` (one hop for the whole fleet) or `dims`",
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
        "status: the session's SUBJECT + classified STATUS (pct-encoded; '-' = unset).",
        "Reply: OK schema=1 sid= subject= subject_source=pin|osc|cwd|unavailable observed= \
         phase=unknown|starting|idle|running|quiet|exited since_ms= \
         outcome=none|success|failure|signal exit_code= signal= detail= \
         confidence=exact|strong|heuristic|unknown reasons= conflict= revision= enabled= \
         hold=<0|1> fabric=<connected|disconnected|absent>. \
         Read-only. `observed=false` means never classified, which is NOT `phase=unknown` \
         (classified, no evidence); `subject_source=unavailable` means the terminal lock was \
         contended, never a silent fall to a lower rung. Fields are ADDITIVE and never bump the \
         schema, so reject an unknown schema MAJOR rather than best-effort parsing, and treat an \
         unknown phase/outcome/reason token as unknown rather than an error. `enabled=false` \
         means `tab_status` is off and every phase will read unknown. `detail=` is the \
         sanitized RUNNING command — the command's FIRST word reduced to its basename, plus \
         an allow-listed subcommand, never an argument (`claude`, `codex`, `targo%20test`); a \
         COMPOUND command's first word is the shell keyword that opens it, so `for i in 1 2 \
         3; do ...; done` reads `detail=for`, not the program inside — while the \
         shell-integration block executes, `-` otherwise or when the terminal lock was \
         contended: how an agent tells that a peer is another agent before typing into it. \
         `hold=1` means a fleet halt is in force for this session, so every PTY-reaching verb \
         answers `ERR halted` — read the reason from `inbox`. `fabric=` is INSTANCE state, not \
         this session's: `absent` = no bridge was ever launched, `connected` = one is serving, \
         `disconnected` = the bridge this instance had is gone, which is itself a held state. \
         No `window=` here: `status` is \
         polled, and the window lives on the main thread, so a per-poll hop would be a \
         latency regression — ask `sessions`/`ls` (one hop for the whole fleet) or `dims`",
    ),
    v(
        "timeline",
        Read,
        Lines,
        Session,
        "timeline [<n>] [since=<id>]: the session EVENT TIMELINE",
        "- one `event <id> t=<ms> kind=<k> ...` line per lifecycle event \
         (spawned/state-change/title-change/cwd-change/meta-change), monotonic ids, drop-oldest \
         ring. The two rows a session records as it is retired — `closing reason= by=` and the \
         final `state-change state=closed` — cannot be asked for here after the close: the sid \
         stops resolving in the same store write (only a request that resolved the session just \
         before that write can still read them). A live `subscribe @<sid> events` watch is what \
         delivers `closing` \
         (as `EVENT <local> closing reason= by=`, ahead of `exited`); `exits` keeps the same facts \
         afterwards",
    ),
    v(
        "metrics",
        Read,
        Status,
        App,
        "render/latency counters [reset|percentiles]",
        "- percentiles: p50/p95/p99 input->application-present-return / \
         output->application-present-return / frame-render distributions; plain line carries \
         max_frame_gap_ms= (worst successful-present-return gap since reset), \
         rust_main_to_first_present_ms= and rust_main_to_first_visible_ms= (Rust entry->the FIRST \
         window's actual reveal — time-to-visible; on a warm Windows launch the reveal precedes \
         the backend join, so it runs well under first-present) plus \
         startup_phase_schema=1/startup_phase_valid= and eight exclusive startup_*_ms phases \
         (router, GUI prepare, winit dispatch, initial surface attach, successful-redraw \
         wait/compose/surface/finalize); startup_attach_schema=1/startup_attach_valid= drills the \
         attach parent into \
         dispatch/prepare/window-create/window-setup/backend-finalize/chrome-geometry/surface-create/finish, \
         effect_pipeline_builds=/effect_pipeline_build_ms= report the EFFECT-only cell pipelines \
         this process compiled ON DEMAND (the nine that only ever draw a cursor trail / fire / \
         rain / sparkle / sprite layer are built the first frame that binds one, never at launch \
         — a default `cursor_trail = false` run reads 0/0.00 for its whole life, and a non-zero \
         reading with no effect enabled means the demand gate has re-eagerised), and the line \
         ends first_present_ms= (compatibility GUI main_entry->the same successful-present \
         publication; dyld/compositor/scanout unobserved) first_visible_ms= (GUI main_entry->the \
         same reveal instant). READ THE SLICES HONESTLY: present_* and input_* are OPEN INTERVALS closed by the next qualifying present, so any stretch in which nothing presented is INSIDE the number (only a 5 s discard bounds it) — a multi-second present_latency means \"nothing presented for that long\", not \"a frame took that long\"; input_* closes on the next CONTENT present, which under concurrent streaming output may be a log-line frame rather than the key's echo, so it reads LOW rather than high. Quote n_* with the percentiles, never a lone last_/max_, and note both stop at application-present return (no compositor selection, scanout or photons)",
    ),
    // drive input
    v(
        "turn",
        Write,
        Lines,
        Session,
        "turn [option=value ...] <text>: ONE HUMAN TURN",
        "- type <text>, verified submit (submit=none types WITHOUT submitting; default 'enter', \
         and any key-verb name is a valid submit key), wait for the screen to settle (idle for \
         idle=<ms>, cap timeout=<ms>), return the settled screen. Options: [idle=<ms>] \
         [timeout=<ms>] [submit=<key|none>] [settle=match:<re>] [submit_window=<ms>] \
         [presses=<n>] [submit_verify=<auto|seq|block>] [trim=<0|1>]. Reply: verdict line \
         (id=/submitted=<0|1>/status=settled|timeout/seq=/dur_ms=/hash=<FNV16 of settled screen>) \
         then the rows; trim=1 drops the trailing all-blank rows and closes the verdict with \
         trimmed=<k> — hash= stays the FNV of the UNTRIMMED screen (a screen identity, the value \
         `history` reports), not of the bytes sent. App-agnostic; humans can interject. \
         EXACTLY-ONCE: a leading id=<epoch>:<producer>:<seq> makes the turn replay-safe — see \
         `send`'s entry for the key, which every input verb shares. A DUPLICATE answers `OK 0 \
         dup=1` (this verb is Lines-framed) and carries NONE of the verdict fields above, id= \
         included: nothing was typed, so there is no verdict to report. The reply's own id= is \
         the TURN id and is unrelated to the key you sent",
    ),
    v(
        "lease",
        Write,
        Status,
        Session,
        "lease [status|acquire|release ...]: a COOPERATIVE drive lease for raw (non-turn) drivers",
        "— one holder at a time, TTL-expiring (default 30000ms, max 600000), surfaced in `who` as \
         driving=lease:<holder>. Forms: lease [status] | lease acquire [ttl=<ms>] [holder=<name>] \
         | lease release [holder=<name>] [force]. acquire refuses a live `turn` or a DIFFERENT \
         holder's live lease (steals a lapsed one; same holder renews); a live lease also blocks \
         a `turn` from stomping it. ADVISORY for raw send/key/feed (use `turn` for HARD \
         arbitration) — the coordination signal cooperating agents check before driving",
    ),
    v(
        "send",
        Write,
        Status,
        Session,
        "write text to the PTY; reply `OK seq=<n>`",
        "— the content baseline, so `await seq <n+1>` waits for the output this input causes \
         (same seq= on key/ctrl/feed/mouse/paste). EXACTLY-ONCE: a LEADING \
         id=<epoch>:<producer>:<seq> stamps the write, and `key`, `feed-bin` and `turn` take the \
         same key. <epoch> is this session's launch nonce (the roster's nonce=); <producer> is any \
         u64 stable per driver; <seq> is that driver's own monotone sequence. A sequence at or \
         below the producer's high-water writes NOTHING and answers the duplicate marker IN THE \
         VERB'S OWN FRAMING — `OK dup=1` for a `Status`-framed verb (`send`, `key`, `feed-bin`), \
         `OK 0 dup=1` for a `Lines`/`Bytes` one (`turn`), because a bare `OK dup=1` would make a \
         Lines client read `dup=1` as a row count — so a driver that crashed without seeing its \
         reply can retry safely. A retry of a sequence whose attempt \
         did NOT answer OK gets `ERR in-doubt seq=<n>` — it may have typed, so it is reported, \
         never replayed, and the session's `timeline` carries the row. A key minted before a \
         relaunch is `ERR epoch`. OPTIONS LEAD: only a FIRST token spelled id= is a key, so \
         `send hello id=1` still types `hello id=1`, and a leading `--` ends option parsing",
    ),
    v(
        "paste",
        Write,
        Status,
        Session,
        "bracketed-paste text to the PTY",
        "the payload rides the engine's PASTE seam — bracket guards when the app has set \
         DEC 2004 AS THE FRAME IS WRITTEN, control-byte sanitize, LF->CR — but NOT the \
         `confirm_multiline_paste` prompt a person's paste answers. A driver that asked \
         for a paste has already decided, and parking this reply on a window banner would \
         hang the caller; so an unbracketed multi-line payload reaches the shell here \
         without the confirmation the same text raises from the keyboard, and its first \
         line can run. Same-uid, token-gated and owner-scoped — whoever can send this can \
         already run commands — so the difference is stated rather than left to be \
         discovered. That is also why the framing is read at WRITE time and not captured \
         earlier the way a keyboard paste's is: a keyboard paste captures the mode its \
         confirmation was judged under, and this verb was never shown one",
    ),
    v(
        "key",
        Write,
        Status,
        Session,
        "send a named key (enter/tab/up/...)",
        "— accepts a leading id=<epoch>:<producer>:<seq> idempotency key (see `send`)",
    ),
    v("ctrl", Write, Status, Session, "send a control char", ""),
    v(
        "feed",
        Write,
        Status,
        Session,
        "write raw bytes to the PTY",
        "",
    ),
    v(
        "hwkey",
        Write,
        Status,
        App,
        "hwkey <char|name> [mods=] [count=] [interval=]: inject a key through the OS event queue",
        "(macOS: a real NSEvent posted to this app), so it takes the SAME winit path a \
         physical keypress takes and carries the NSEvent-queue backdate. Use this — NOT \
         `key` — to measure typing latency: `key` posts straight to the main thread and is \
         born already dequeued, so it cannot see OS-level key queueing (the drawable park) \
         at all. Replies `OK posted=<n>` — events handed to the OS queue, not bytes \
         written; read the result with `metrics percentiles` (`n_key_write` moves only on \
         this path)",
    ),
    v(
        "feed-bin",
        Write,
        Status,
        Session,
        "length-prefixed raw bytes to the PTY",
        "`feed-bin <n> [id=<key>]` then <n> raw bytes. The optional idempotency key (see `send`) \
         rides the HEADER, not the payload: a frame whose key is refused still consumes its \
         announced bytes, so the stream stays framed and the next request line is the client's",
    ),
    v(
        "paste-bin",
        Write,
        Status,
        Session,
        "length-prefixed bytes to the PTY with PASTE semantics",
        "(bracketed-paste guards as DEC 2004 stands when the frame is written + control-byte \
         sanitize + LF->CR); the binary twin of `paste`, framing contract included",
    ),
    v("mouse", Write, Status, Session, "inject a mouse event", ""),
    // `pointer` is `Write` because it drives the WINDOW's pointer through the very
    // entry point winit's `CursorMoved` calls, and under DEC 1000/1002/1003 that
    // motion is REPORTED to the driven program — `Op::WriteInput`'s own doc lists
    // the mouse in the human vocabulary. It deliberately reports NOTHING derived
    // from the grid: `link=` here would hand a keystroke-only edge a fact only
    // `cell` (`Read`) is entitled to answer, and write does not imply read.
    v(
        "pointer",
        Write,
        Status,
        App,
        "pointer [move <r> <c>|leave|status]: put the POINTER on a cell, so hover resolves",
        "— `mouse move` posts an engine `InputEvent` and never touches the window's pointer, so \
         nothing it does makes a link hover, a divider cursor or a tab-strip highlight happen. \
         This drives `App::on_cursor_moved` — the identical function `WindowEvent::CursorMoved` \
         calls — after mapping the cell to the centre pixel of the frame it is drawn in, and \
         `pointer leave` drives `on_cursor_left` (what `WindowEvent::CursorLeft` calls). Reply: \
         `OK at=<row>,<col>` is where the pointer ACTUALLY resolved, read back from the window \
         AFTER the real path ran, in window (not pane-local) cells — so it states where the \
         pointer IS rather than repeating the request. A cell outside the grid is `ERR`, never \
         silently clamped onto a neighbour and reported as though it had been honoured. `OK at=-` \
         when the window holds no pointer position at all: after `pointer leave`, and before the \
         first move of any kind. `at=-` says only that — never a position that does not exist. \
         Read the hover the pointer resolved with `cell <r> <c>` (`link=`) and see the destination \
         band with `image`. FRONT window of the resolved instance, like `hover`",
    ),
    v(
        "resize",
        Write,
        Status,
        Session,
        "resize <r> <c>: resize the engine + PTY (grid first, window echoed to match).",
        "`resize px <w> <h>` instead resizes the WINDOW in physical pixels and lets the grid \
         follow from the platform resize event — the same path an edge drag takes, so it is the \
         form that exercises the live-resize width throttle (the cell form pre-applies the grid, \
         so the window event never sees a column change). Drive several back to back to reproduce \
         a drag's event pressure; read the result with `metrics` (`resize_present`) and `dims` \
         (`layer_*`)",
    ),
    v("focus", Write, Status, Session, "send focus in/out", ""),
    v(
        "scroll",
        Read,
        Status,
        Session,
        "scroll the view (up|down|top|bottom|N)",
        "",
    ),
    v(
        "select",
        Read,
        Status,
        Session,
        "select a region / word <r> <c> / line <r> / clear",
        "",
    ),
    v(
        "signal",
        Signal,
        Status,
        Session,
        "signal <sig>: send a signal to the foreground process group",
        "",
    ),
    // app / GUI drive (selector routes to the instance's front window)
    v(
        "tab",
        Write,
        Status,
        App,
        "drive a window's tabs (new|N|next|prev|close [N]|move <from> <to>)",
        "- flagless drives the FRONT window; `@<sid> tab …` drives the window hosting <sid> \
         (the same aim as `@<sid> spawn`), so an agent in a background window walks its own \
         tabs without touching the human's. Replies `OK <active> <count>`; a --headless \
         instance drives its one logical window like a real one (no `ERR headless`)",
    ),
    // `pane` is `tab`'s within-a-window twin and carries `tab`'s class for `tab`'s
    // reason: choosing which pane the keyboard drives is an input-routing authority,
    // so a read-only edge must not be able to redirect the human's next keystroke.
    // Its reply is one bit about the caller's OWN act — strictly less than the tab
    // COUNT `tab` already hands a write edge — so it needs no read authority beside
    // it; `panes` (`Read`) is what answers where focus went and what the rects are.
    v(
        "pane",
        Write,
        Status,
        App,
        "pane <left|right|up|down>: move keyboard focus to the adjacent pane",
        "- the ⌘⌥-arrow / `focus_pane_*` binding's own path, so the active-pane mark moves with \
         it and the window re-mirrors term/master/socket onto the newly focused pane exactly as a \
         click-to-focus does. `spawn split=v|h` makes the panes; this is what then walks them. \
         Reply `OK moved=1` when focus changed and `OK moved=0` when it did not — a single-pane \
         tab, or no neighbour that way — which is a fact about the caller's own request, not a \
         reading of the layout: ask `panes` (read-side) for the rects and which pane is focused. \
         FRONT window of the resolved instance, like `hover` — it does NOT aim at the window \
         hosting an `@<sid>`, the way `tab` and `spawn` do",
    ),
    v(
        "open",
        Write,
        Status,
        App,
        "open a native tab app: `open app settings [/route]`",
        "or `open app markdown|editor <local-file-uri>`; compatibility aux targets remain available",
    ),
    v(
        "act",
        Write,
        Status,
        App,
        "dispatch an exact semantic native-app action: act app/v1 view <view-id> <ui-key> <action>",
        "[value]",
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
        "",
    ),
    v(
        "invoke",
        Write,
        Status,
        App,
        "invoke <action>: fire a menu action by name (enabled-gated; names via `controls menu`)",
        "",
    ),
    // Runtime-only per-session visual toggle (nothing durable is written); the
    // observability face scripts/tests read (`status` = one Status line).
    v(
        "rain",
        Write,
        Status,
        App,
        "rain [status|on|off|toggle]: matrix rain for the focused window's front session",
        "(status prints config_enabled= session_override= effective= engine= active= \
         scope=window|focused-pane focused= animating=, plus a live engine's weather= density= \
         tick= scanned= material= emitting= vis= drain= seq= streak= diag)",
    ),
    // Read-only observability for an effect that is otherwise nearly INVISIBLE
    // by design: PRISM WAKE answers program output with a ~0.10-coverage comet
    // that lives under a second, so "off", "W11-demoted", "suppressed" and
    // "resting" all look identical on glass. No write form — the knobs are
    // durable config (`settings set output_streak.…`).
    v(
        "streak",
        Read,
        Status,
        App,
        "streak [status]: output-streak (PRISM WAKE) state for the focused window",
        "(prints config_enabled= sound_key= motion_animates= focused= fx_focused= \
         serious_sound= sounds_master= volume= engine=none|live panes= active= intensity= \
         tail= max_streaks= idle_secs=; `motion_animates` folds the W11 unfocused \
         demotion, which outranks even motion=full; `fx_focused` is the focus input the \
         render tick actually uses (raw focus OR a live typed wake OR an in-flight \
         recording), so a window can animate with focused=false; `panes` counts the \
         composed path's per-pane engines, which is where a SPLIT's streaks live)",
    ),
    // Read-only observability for an effect that is otherwise audible-only: the
    // tone-of-typing mood steering the trail synth's melody. No write form —
    // the knob is durable config (`settings set tone_melody`).
    v(
        "tone",
        Read,
        Status,
        App,
        "tone [status]: tone-of-typing state for the focused window",
        "(prints tone= effective= knob= sounds= volume= audio=live|wedged|inert active= \
         window_chars= inferences= dropped=; `effective` is what the synth is stamping, \
         `inferences` separates \"the model ran and said technical\" from \"the model never \
         ran\", audio=wedged means the audio worker is stuck inside one platform call and cues \
         are being dropped (dropped= counts them). The typed window's TEXT is never reported)",
    ),
    // Read-only observability for the cursor-trail engine: the last N
    // licensed/declined verdicts from the fixed-size admission diagnosis ring,
    // and (`trail status`) the engine's standing state. The one-command face
    // of what the rainbow-trail blackout hunt did with ATERM_TRACE_SPAWN
    // stderr logs, and of what "I don't see the rainbow cursor trails" needed a
    // video recording for.
    v(
        "trail",
        Read,
        Lines,
        App,
        "trail [<n>]: the focused window's last <n> cursor-trail spawn-seam verdicts",
        "(default all, ring cap 32), newest last — one `admission seq= phase=licensed|declined \
         reason= age_ms= origin= target= alt=` row per judged cursor move, from the engine's \
         fixed-size DIAGNOSTIC ring. A move paints only if a keypress LICENSED it, so a \
         decline carries one of four reasons: `no-fresh-hint` (no key hint was fresh — the \
         move was program output nobody's fingers asked for), `no-credits` (a multi-cell \
         coalesce outran the press CREDIT budget), `off-shape` (licensed and classified, but \
         the style's shape gates laid nothing), `program-row` (the anchored-echo lane refused \
         a row that has advanced keylessly, or one contesting a fresher row's echo — a \
         spinner/status row must never spend a typed stamp). Every observed move is counted \
         exactly once: \
         licensed + declined is the number of cursor deltas the seam has judged. \
         `trail status`: one standing-state row instead — `trail style= resolved= \
         config_enabled= effective= focused= motion= motion_stage= shed= intensity= \
         licensed= declined= last_decline_reason= spawns= ribbon_active= ribbon_look= \
         ribbon_segments= ribbon_hue_bands= field= field_span= sparks= momentum= \
         momentum_display= speed= resume_grant= woken= bloom= \
         glow_active= pet_active= cat_active= \
         block_fill= block_fill_rgb= block_fill_base= block_fill_base_from=` (every gate \
         from the config knob to the glass, in the order the frame path walks them, plus \
         the cumulative tally the ring has forgotten — `licensed=0 declined>0` blames the \
         licence and names why, `licensed>0` over a dark screen blames everything \
         downstream of it). The `block_fill*` four are the BLOCK CURSOR's body, which no \
         other field covers: a style can take the caret away from the terminal entirely, \
         and `glow_active=false pet_active=false` over a tinted block is what that looks \
         like from every other gate. `block_fill=` names the owner the frame actually \
         painted (`rainbow`/`forge`/`phaser`/`bolt`/`comet`/`droplet`/`beamrod`, or `none` \
         for the terminal's own cursor colour), `block_fill_rgb=` is the hex it drew, and \
         `block_fill_base=`/`block_fill_base_from=` are the colour that body was built \
         FROM and which knob supplied it (`cursor_color`/`trail_color`/`style_identity`) — \
         so a caret that ignored OSC 12 is separable from one that honoured it. Read-only; \
         typed text is never reported",
    ),
    // Read-only observability for SELECTION/VIEWPORT CUSTODY: which of the eleven
    // custody-moving events last fired. Several of them leave identical state behind
    // (an auto-repeat tick, a bare modifier and a key release each change nothing at
    // all), so `scroll` and `selection` cannot answer "why did my selection
    // disappear?" between them — only the engine's own record can. No write form: the
    // record is written by the seams that make the decision.
    v(
        "custody",
        Read,
        Status,
        Session,
        "custody: why the reading position or the highlight last moved",
        "— one `last=<transition|none> event=<0-7|-> changed=<transition|none> offset= \
         owner=user|tail selection=yes|no scrollback=` line naming the PressCustody transition \
         the engine recorded: a press (TypingPress, RepeatPress, InertPress, ReleaseEvent), a \
         gesture (UserScroll, UserSelect, UserClear), or output (OutputAtLive, \
         OutputWhileReading, OutputDamagesTheSelectedRows, OutputInvalidatesTheCoordinateSpace, \
         OutputTookTheSelectionUnattributed). `last` is the most recent event of any kind; \
         `changed` is the most recent one that actually TOOK the offset or the highlight, so \
         ordinary shell output cannot bury the answer. `none` means nothing has moved custody \
         yet. Read-only; reports no screen content",
    ),
    v(
        "hover",
        Write,
        Status,
        App,
        "toggle the drop-target highlight",
        "",
    ),
    // session lifecycle
    v(
        "spawn",
        Write,
        Status,
        App,
        "spawn [window=<id>] [raise=<true|false>] [cwd=<path>] [split=<v|h>] [connected=...]:",
        "mint a new session (a tab, or split the focused pane), reply OK <sid> - immediately \
         addressable. AIM it with window=<id> (an id from `inspect app/v1 tabs`) or `@<sid> \
         spawn` (the window hosting <sid>; window= wins); split= then divides THAT window's \
         focused pane. raise= defaults to true when no window was named (the `aterm new-tab` \
         attach contract) and FALSE when one was - an agent aiming at a background window is \
         not asking to see it; say raise=true to insist. Unknown id: `ERR no such window <id>`. \
         A --headless instance owns logical window 0 (the one `ls`/`windows`/`dims` name), so \
         `window=0` and `@<sid>` aim there exactly as at a real window - only `window=<other>` \
         is `ERR no such window` - and the raise is simply a no-op (no OS surface). The \
         connected form, `spawn \
         connected=controlled|controller place=window|tab of=<sid> [cwd=<path>]`, also mints a \
         `both` session connection with of=<sid> (controlled: of= drives the newborn; \
         controller: the newborn drives of=, its shell gets ATERM_OBSERVE_SESSION_ID) - \
         Owner-only, of= mandatory, no window=/raise=/split= beside it, place=window is `ERR \
         headless` with no GUI",
    ),
    v(
        "close",
        Write,
        Status,
        Session,
        "@<sid> close: retire that session (close its tab) - the death half of spawn",
        "`OK closed <sid>` means the session LEFT the registry: its tab is gone, its PTY was hung \
         up, and the ledger has the row (`exits` -> `reason=ctl-close by=<caller>`). An \
         unresolvable sid, or one no window holds, is `ERR no such session`. `ERR close refused \
         (a running job armed the last-tab confirm)` = the destructive-close confirm did not let \
         a LAST-tab close through and the tab is still there (a `--headless` instance never \
         confirms, so it never answers this). Closing a window's LAST tab DEFERS the window \
         teardown that retires the session (that teardown needs the event loop), so the verb \
         waits for the escalation before answering; if the session is still registered after it, the \
         reply names WHICH deferral it is looking at: `ERR close deferred (the window teardown \
         has not run)` - its window is still standing, a native or document close barrier held \
         it; `ERR close deferred (another window still displays this session)` - a Cmd-Shift-O \
         co-viewer still holds a pool view, so retiring this tab did not retire the session (the \
         torn-down window is gone, so a repeat aims at the next viewer: one close per viewer); \
         `ERR close deferred (the session outlived its window's teardown)` \
         - the window is gone and no window shows the session. `ERR session view changed during \
         close` = the tab was a heterogeneous one (a terminal beside a native view) and its \
         shape moved under the close; nothing was retired, so retry. The verb takes NO \
         argument: anything after it is `ERR usage: close`, never a close. And when that LAST tab is the \
         LAST window's, the teardown ENDS THE PROCESS: `OK closed <sid>` is this instance's \
         final reply, the memory-only `exits` ledger and any `subscribe` watch's `closing` / \
         `exited` frames go with it, and the next verb gets a connect error, not an `ERR` line",
    ),
    // waits
    v(
        "ready",
        Read,
        Status,
        Session,
        "block until the session is alive and idle",
        "",
    ),
    v(
        "await",
        Read,
        Status,
        Session,
        "await <idle|seq|match|block|inbox|consent> [args] [timeout=<ms>]: block until one latches",
        "Full grammar: `await idle <ms>` (no output for that long), `await seq [<n>]` (the \
         content sequence passed <n>; bare = the next change, so `await seq <n> timeout=0` is the \
         cheap dirty check), `await match <re> [rows <a> <b>]`, `await block` (the running \
         command completed), `await inbox since=<id> [kinds=<k,...>]` — the FABRIC predicate: \
         it latches on an inbox row with id > `since` of an accepted kind (default: every kind \
         but `note`), or on a `hold` transition when `hold` is one of the listed kinds. Monotone, \
         so a row the agent chose to ignore cannot latch the same wait twice. And `await consent` \
         — the macOS privacy posture of THIS session: the instance's Full Disk Access state, this \
         session's `fs_consent=` and its `attribution=` (see `privacy`). It ARMS when the request \
         arrives, taking that tuple as its baseline, and latches on the first observed change \
         from THAT baseline: never on a value that was already true when you asked, and never \
         against a baseline captured earlier. Its default `timeout=300000` is finite on purpose — \
         the system consent dialog it waits behind never expires, so an agent must not park on an \
         absent human forever; a timeout there is an ordinary timeout reply, not an error. A \
         latch says aterm's own posture CHANGED, not that a human \
         answered a dialog (aterm cannot observe the answer), and a change becomes observable \
         within one probe interval plus a polling tick. Every form answers `OK <predicate> <seq>` \
         on a latch and `OK timeout` otherwise, which the client exits 124 on. One control lane \
         per parked wait, so park at most one per driver.",
    ),
    v(
        "wait",
        Read,
        Status,
        Session,
        "block until the running command completes (OSC-133)",
        "",
    ),
    // streaming
    v(
        "subscribe",
        Read,
        Push,
        Session,
        "subscribe @<sel>[,...] <streams> [since=][every-frame]: push DELTA/EVENT/GAP/BYTES;",
        "streams=screen,cursor,cells,bytes,events,sessions, at least one of them (a modifier-only \
         list is `ERR usage`); events = the per-target digest (`EVENT <local> turn|block-complete|\
         meta|title|bell …`, then, as the session is retired, `EVENT <local> closing reason= by=` \
         — the `exits` row, and this watch is the only wire path that carries it — before its \
         one `EVENT <local> exited`, not necessarily adjacent: a title or bell frame of the same \
         watch can land between the two); sessions = instance lifecycle (`EVENT * \
         session-created <sid>` / \
         `EVENT * session-exited <sid> reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|\
         unknown>` for sibling spawns/exits, no `ls` polling — the reason is the `exits` ledger's, \
         a trailing additive token; `app-quit` is reserved, not produced today) and is OWNER-ONLY \
         because it reports \
         the whole roster, not just your targets — a scoped edge asking for it gets `ERR denied`; \
         add `timestamps` (alias `ts`) INSIDE <streams> (`cells,ts`; trailing is `ERR unknown \
         subscribe arg`) to prefix frames with `T <local|*> <t_us>` lines (video's clock) so the \
         stream is a timed frame source — at most one per channel per wake, tagged `<local>` for \
         session frames and `*` for `sessions` events, so the second token is not always numeric; \
         add `trim` INSIDE <streams> too (`screen,trim`; trailing is `ERR unknown subscribe arg`) \
         to stop each screen DELTA after its last non-blank row — `screen <nrows>` is then the \
         count sent (inert without screen)",
    ),
    // fabric messaging — the per-session INBOX RING, this session's outbound posts,
    // and the two BRIDGE-plane verbs. `inbox`/`inbox get`/`inbox seen`/`post` are
    // ordinary scoped verbs an agent inside the session calls; `deliver`/`hold` are
    // `BridgeOnly`, so no token reaches them (see [`Access::BridgeOnly`]).
    v(
        "inbox",
        Read,
        Lines,
        Session,
        "inbox [<n>] [since=<id>] [--peek] [--meta]: this session's message rows",
        "Header `OK <n> hold=<0|1> holder=<p|-> seen=<id> bus_head=<off> dropped=<n> pending=<n>`, \
         then one `msg <id> off=<n> t=<ms> from=<p> kind=<k> \
         trust=<human|agent|relayed|screen> [re=<n> re-id=<id>] [dl=<ms>] [late=1] [demoted=<k>] \
         [via=<p,...>] len=<n> [more=1] [truncated=1] text=<pct>` row per message and one `post \
         <id> to=<> \
         kind=<> off=<n|->` row per outbound post that has not landed yet. `trust=` is the \
         RECEIVER's verdict on what the content is, never a sender's claim, and it is printed \
         before the text on purpose. `text=` is pct-encoded and cut at 512 B with `more=1`; \
         `inbox get <id>` returns the whole of what this endpoint HOLDS. `truncated=1` means the \
         endpoint never received the rest: the delivering bridge cut the body to fit one control \
         line and `len=` names the true size, so that message is NOT recoverable in full here — \
         a cut row carries `more=1` too, even when what survived is under 512 B. `dropped=` \
         counts UNHANDLED rows the bounded ring evicted (never silently) — every evicted row \
         above `seen=`, not merely one nobody listed — and `pending=` the delivered rows this \
         reply did not carry. A bare `inbox` advances the LISTED watermark — what the ring counts \
         as read for eviction and for the per-peer quota — while `--peek` moves nothing and \
         `--meta` omits `text=`; the HANDLED watermark `seen=` moves only on `inbox seen`, which \
         also LISTS every row at or below its argument (see that entry).",
    ),
    v(
        "inbox get",
        Read,
        Bytes,
        Session,
        "inbox get <id>: one message's body as this endpoint holds it, length-prefixed",
        "`OK <nbytes>` then that many raw bytes (up to 256 KiB) — the un-PREVIEWED form of the \
         `inbox` row's `text=`, which the row cuts at 512 B. Reading a body moves no watermark. \
         NOT ALWAYS THE WHOLE MESSAGE, and it says which: a body the delivering bridge had to cut \
         to fit one control line is answered `OK <nbytes> truncated=1 len=<true-size>`, and the \
         missing bytes are NOT recoverable by any verb — they are on the bus, which the recipient \
         has no access to. Only the first token after `OK` is the frame length, so the marker \
         does not change the framing.",
    ),
    v(
        "inbox seen",
        Write,
        Status,
        Session,
        "inbox seen <id> [handled|refused|deferred]: advance the HANDLED watermark",
        "`OK seen=<id>`, and pushes `EVENT <local> inbox-seen <id> off=<n>` on the events digest. \
         It ALSO LISTS every row at or below `<id>`, which is a second effect and not a side \
         effect: listing is what the ring counts as read for eviction and what RELEASES the \
         sender's per-peer quota, so an agent that only ever `--peek`s can still acknowledge its \
         mail and keep receiving. Write-gated exactly like `meta set`: it records a decision and \
         reaches no PTY, so it stays answerable while a fleet `hold` is on — a halted agent must \
         still be able to mark the notice read.",
    ),
    v(
        "post",
        Write,
        Status,
        Session,
        "post to=<@<sid>[@<node>]|<principal>|say> kind=<k> [opts] <text>: send a message",
        "kind is `ask|answer|task|report|note|ack|control`; `re=<n>` names the offset being \
         answered, `dl=<ms>` is an advisory deadline, `via=<p>` marks a relay, and `--wait[=<ms>]` \
         (ON by default for `ask` and `task`) blocks until the bridge reports the record landed \
         and answers `OK <id> off=<n>` — the broker-assigned offset is the correlation id an \
         answer carries back as `re=`. When the link cannot report a landing the wait ends at \
         once with `ERR fabric <absent|disconnected> id=<n> queued=1`: `queued=1` says the \
         message is STILL IN THE OUTBOX and a replacement bridge will publish it (a bridge exit \
         is the ordinary relaunch path, and `outbox` is a peek that removes nothing), so it must \
         not be read as `not sent` and re-posted — `post` carries no idempotency key that would \
         collapse the duplicate. The body is inline text up to 4 KiB, or `len=<n>` followed \
         by that many raw bytes up to 256 KiB. There is no `to=fleet`: a node holds no fleet \
         write grant, and an agent may only ASK a human to halt. REFUSED to an edge-token \
         connection — a write-input edge over one session would otherwise speak AS that session, \
         and the sender of a post must be attested by the instance Owner. Exempt from `hold`.",
    ),
    va(
        "deliver",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "deliver <sid> off=<n> from=<p> kind=<k> ...: put one bus record in a session's inbox",
        "BRIDGE-ONLY: only the fabric bridge connection may call it, Owner included — see the \
         `hold` entry for why. Idempotent on `off=` over the last 1024 delivered offsets: a \
         redelivered offset answers the id it first got and appends nothing, which is what turns \
         the bus's at-least-once cursor into exactly-once at the endpoint — and NOT one offset \
         further, because the dedup window is twice the 512-row ring. A redelivery older than \
         that reappears as a FRESH row (the row was dropped unread and the bus still holds it), \
         which a session that lists its mail without ever running `inbox seen` can reach: the \
         bridge refills from the persisted `seen=` offset. `ERR quota` past 64 unread rows from \
         one ATTESTED PEER — counted on the cap-forced `<src>` (the part after `@`, or the whole \
         `from=` when there is none) and NOT on the whole `from=` string, half of which the \
         sending node chooses — so one peer cannot evict a human's unread `task` under a burst of \
         `note`s by rotating pseudo-sids, and eviction never drops an `h-*` row ahead of an \
         agent's. `deliver <sid> landed=<post-id> off=<n>` is the \
         other form: it closes an outbound `post` and releases its `--wait`.",
    ),
    va(
        "hold",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "hold <sid> on|off [reason=<pct>] [origin=fleet|local]: the fleet drive halt",
        "`OK hold=<0|1>`. BRIDGE-ONLY, and that is the whole point: Owner scope is what every \
         in-session client already holds, so a halt an injected agent could lift locally would be \
         no halt at all. While on, every PTY-reaching verb resolving to that session answers `ERR \
         halted <reason>` from ANY scope — `send key ctrl feed feed-bin paste paste-bin mouse \
         resize focus signal turn close invoke hwkey pane tab operator-propose-bin` — a TRANSIENT class \
         beside `ERR busy`, so existing back-off code already does the right thing. `focus` is in \
         that set because it writes the DEC 1004 focus reports to the PTY; `invoke` is, because \
         `invoke Paste` writes the clipboard into the front tab's PTY; `tab` is, because `tab \
         close [N]` RETIRES a session exactly as `close` does, and a driver refused `close` used \
         to type that instead. `invoke` and `tab` resolve no session, so they are refused while \
         ANY session on the instance is held. `post`, `inbox seen`, \
         `meta set`, `lease` and every read verb stay answerable, and the physical keyboard is untouched: a \
         halt stops drivers, not humans. When the bridge connection closes, the instance holds \
         every session that bridge ever delivered to or held with `reason=fabric-lost \
         origin=fleet` — the halt must not depend on a killable process staying alive.",
    ),
    va(
        "outbox",
        Read,
        Bytes,
        Meta,
        BridgeOnly,
        "outbox [<max>]: drain the queued outbound posts, bodies included",
        "BRIDGE-ONLY, and the mirror image of `deliver`: `deliver` is how a record enters the \
         instance, `outbox` is how one leaves it. `OK <nbytes>` then that many raw bytes, holding \
         one `post sid=<s> id=<n> to=<pct> kind=<k> [re=<n>] [dl=<ms>] [via=<p,...>] len=<n>` \
         line per queued post followed by that post's `len` body bytes — a length prefix and not \
         a row, because a body may contain newlines and a line-framed listing could not carry it. \
         A PEEK: it moves no watermark and drops nothing, so a bridge that dies mid-publish \
         re-reads the same posts on restart and republishes them under the same producer \
         sequence. `outbox sent` is what retires one. One drain is BOUNDED IN BYTES as well as \
         by `<max>` (4 MiB across all sessions, always at least one post so a large body is \
         never stranded): the per-session queue bounds are per session, and one reply used to \
         concatenate every session's bodies. A drain stopped by the budget is resumed by the \
         next call, which is safe precisely because nothing was retired. Owner-forbidden for the same reason \
         `deliver` is: an Owner-token connection reading this would read every session's \
         outbound traffic.",
    ),
    va(
        "outbox sent",
        Write,
        Status,
        Meta,
        BridgeOnly,
        "outbox sent <sid> <id> off=<n|->: retire one queued outbound post",
        "BRIDGE-ONLY. `off=<n>` is the broker offset the post landed at: it fills the `post` \
         row's `off=`, releases a `post --wait` parked on it, and lets the endpoint drop the \
         retained body — the body is kept only until this arrives, which is what bounds the \
         queue's memory. `off=-` retires it as permanently undeliverable instead, with an \
         `undeliverable` row explaining why arriving separately through `deliver`. An optional \
         `reason=<word>` on the `off=-` form names WHICH refusal it was, and a `post --wait` \
         parked on that post wakes with `ERR <reason> id=<n>` instead of a uniform `ERR \
         undeliverable`: routing is the bridge's knowledge, not the endpoint's, so `ERR \
         ambiguous` for a sid two nodes claim can only reach the sender this way. Idempotent: \
         retiring a post twice is `OK`, not a second event, so a bridge that retries after a lost \
         reply cannot double-push. A connection that could forge this would release a `post \
         --wait` for a message that never left the machine.",
    ),
    // sessions, presence & capability — the OwnerOnly access declares the owner
    // gate IN the table (the dispatch reads it, no hardcoded verb list). `who`
    // keeps its `Read` op-class (a fleet-wide presence readout) yet is Owner-gated:
    // op-class and scope-gate are orthogonal, which is exactly why `access` is a
    // separate field.
    va(
        "operator",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "embedded opt-in operator: \
         status|inspect|manage|unmanage|next|extend|ack|reconcile|clear-fault",
        "(Owner-only)",
    ),
    va(
        "operator-propose-bin",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "length-prefixed JSON proposal on stdin for the embedded operator actuator (Owner-only)",
        "",
    ),
    // `sessions` is the fleet roster. Its trailing tokens are ADDITIVE: `meta=`
    // (stage 1), then `window=`/`active=`/`wfocus=` from ONE main-thread hop per
    // call (F2: windows on the session verbs) and `detail=` from each engine's
    // executing block (F5: who lives in a session). Old clients key on the sid
    // field and ignore the tail.
    va(
        "sessions",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "OK <n> + per session: local sid parent state title meta= nonce= window= active= wfocus= detail=",
        "Owner-only. `nonce=<hex32>` is the session's PUBLIC launch nonce — the freshness fence \
         an edge binds to and the fabric's `epoch=` verbatim; it is here because `whoami` reports \
         only the connection's own session, so a bridge could not read any other's. \
         `window=<id|none|->`: the hosting window (the `dims` rule — the front \
         window when it shows the session, else the lowest window id); `none` = a session no \
         window holds; `-` = the main thread could not be asked (the line is still printed). \
         A `--headless` instance owns one logical window, id 0 — the one `dims` reports as \
         `window=0 geometry=headless` — so its sessions read `window=0`, never `none`. \
         `active=<1|0|->`: on that window's active tab. `wfocus=<1|0|->`: that window is \
         aterm's MOST RECENTLY FOCUSED window — set when a window gains focus and never \
         cleared on blur, minimize or app deactivate, so exactly one window reads `1` for as \
         long as aterm runs (it is NOT \"the OS key window right now\"). `detail=<pct|->`: the \
         sanitized RUNNING command — the command's FIRST word reduced to its basename, \
         plus an allow-listed subcommand, never an argument (`claude`, `codex`, \
         `targo%20test`); a COMPOUND command's first word is the shell keyword that opens \
         it (`for i in 1 2 3; do ...; done` reads `for`), not the program inside. The same \
         value `status` carries; `-` when idle or the engine lock was contended. One main-thread hop per call, not per session; the client `ls` \
         relays these lines verbatim and `windows` folds them per window. The menu-bar \
         status item's fleet scan reads this on every open under a 2 s per-peer budget, so \
         a peer whose main thread cannot answer inside it drops out of that menu rather \
         than being listed from the registry alone — one hop per open, never a poll",
    ),
    va(
        "who",
        Read,
        Lines,
        Meta,
        OwnerOnly,
        "PRESENCE: per session driving=<turn-id|-> watchers=<n> turns=<n> - the hand + the eye.",
        "Owner-only",
    ),
    // `exits` reads the instance's roster journal — every sid the instance ever
    // hosted — so it is Owner-gated like `sessions`, whose past it is.
    va(
        "exits",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "exits [<n>] [since=<id>]: the instance EXIT LEDGER - why each session went. Owner-only",
        "- one `exit <id> t=<ms> sid=<sid> local=<n> \
         reason=<shell-exit|ctl-close|ui-close|window-close|app-quit|unknown> exit_code=<n|-> \
         by=<sid|human|->` line per session that left the registry (`app-quit` is reserved, not \
         produced today: quit ends the process, ledger included, and deregisters nothing), \
         oldest-first, monotonic ids, \
         drop-oldest ring (`OK 0` = none retained); `<n>` keeps the newest n, `since=<id>` keeps \
         ids strictly greater (page with the last id you saw); the ring is MEMORY-ONLY and \
         per instance - nothing is written to disk, so a `close` that retires the LAST session \
         of the LAST window ends the process and takes the whole ledger with it (read it \
         before that close, not after); `t=` is the `timeline`/`history` \
         clock; `by=` is the closing CALLER: an edge-scoped client's own sid (the session its token \
         was granted to), `human` for a UI/window close, `-` when the connection carried no session \
         identity (an owner-token client is anonymous; never the front tab, never the closed \
         session); `exit_code=-` = \
         the child was hung up by a close, died by signal, was not aterm's to reap, or had not \
         yet exited at either of the ledger's two non-blocking looks. The same facts reach a live \
         watcher as they happen: a `subscribe @<sid> events` watch on the closing session gets \
         `EVENT <local> closing reason= by=` ahead of its `EVENT <local> exited`, and a \
         `subscribe … sessions` watch gets `EVENT * session-exited <sid> reason=`; the `timeline` \
         verb cannot be asked for the `closing` row after the close (the sid stops resolving in \
         the store write that records it; only a request that resolved the session just before \
         that write can still read it)",
    ),
    va(
        "whoami",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "OK <session> <nonce> <scope>",
        "",
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
        "",
    ),
    v(
        "edges",
        Read,
        Lines,
        Session,
        "inbound capability edges (--json). alias: grants",
        "",
    ),
    v(
        "grants",
        Read,
        Lines,
        Session,
        "inbound capability edges (--json). alias: edges",
        "",
    ),
    va(
        "grant",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "mint a cross-session edge (Owner only)",
        "",
    ),
    va(
        "revoke",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "revoke a cross-session edge: revoke <edge-hex> removes one;",
        "revoke src=<sid> sweeps every edge from that source and replies OK <removed> (Owner only)",
    ),
    // Session connections (design §6): the connection-grain verbs over the
    // `grant`/`revoke` op-level primitives. All Owner-only, all self-scoped —
    // the endpoints ride as `dst=`/`src=` ARGUMENTS, never a selector.
    va(
        "connect",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "connect dst=<sid> src=<sid> [kind=pull|push|both]:",
        "declaratively SET the session connection src->dst (mint the missing ops, revoke the \
         excess, so the rows equal exactly kind; default both), reply `OK read-screen=<hex> \
         write-input=<hex> signal=<hex>` with only the minted ops present (Owner only)",
    ),
    va(
        "disconnect",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "disconnect dst=<sid> src=<sid> [kind=pull|push|both]: dissolve the session connection \
         src->dst",
        "(kind-filtered ok: kind=pull revokes only the pull half), reply OK <removed> (Owner only)",
    ),
    va(
        "flows",
        Owner,
        Lines,
        Meta,
        OwnerOnly,
        "the instance's aggregated session-connection graph:",
        "OK <n> + one `<src> <dst> <op>` row per live edge across EVERY session's table (--json \
         groups per pair: {\"flows\":[{src,dst,ops:[..]}]}). Owner-only",
    ),
    va(
        "raise",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "raise <sid>: raise the window hosting that session and select its tab",
        "(the session-connection Show twin; Owner only)",
    ),
    va(
        "dial",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "dial <name>: relay this connection over TLS to the saved network-drive peer <name>",
        "- subsequent verbs run on the remote (owner-only; a pre-relay failure answers one ERR \
         line, success sends no local reply)",
    ),
    va(
        "dial-list",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "list saved network-drive connections",
        "",
    ),
    va(
        "dial-token",
        Owner,
        Status,
        Meta,
        OwnerOnly,
        "token for a saved network-drive connection",
        "",
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

/// Whether `verb` is non-sensitive build/meta provenance or instance posture,
/// answered for ANY authenticated scope BEFORE target resolution
/// (`version`/`update`/`help`/`verbs`/`privacy`).
#[must_use]
pub fn is_any_scope_meta(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::AnyScopeMeta))
}

/// Whether `verb` is BRIDGE-ONLY per the table — only the fabric bridge connection
/// may run it, and no token unlocks it, Owner included. The dispatch reads THIS (not
/// a hardcoded verb list), so classifying a verb `BridgeOnly` in the table is what
/// gates it. Unknown verbs are not bridge-only (`ERR unknown verb`).
#[must_use]
pub fn is_bridge_only(verb: &str) -> bool {
    spec(verb).is_some_and(|s| matches!(s.access, Access::BridgeOnly))
}

/// Trailer emitted after a complete guarded response. Its unpredictable nonce
/// is generated by the server only after receiving the request, so a client
/// cannot pipeline a valid acknowledgement before consuming the response.
pub const ARTIFACT_REPLY_CHALLENGE_PREFIX: &str = "ACK-CHALLENGE ";
/// Client echo sent only after consuming the complete response and challenge.
pub const ARTIFACT_REPLY_ACK_PREFIX: &str = "ACK ";

/// Maximum UTF-8 bytes in one control-protocol reply line. The shipping client
/// enforces this on every line; producers of intentionally large single-line
/// payloads use the same ceiling so they never emit a reply their own client
/// must reject.
pub const MAX_CONTROL_REPLY_LINE_BYTES: usize = 8 << 20;

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
    let image_bytes = req_no_sel
        .split_whitespace()
        .skip(1)
        .any(|token| token == "--bytes" || token == "bytes");
    match verb {
        // File captures retain exact-name guards. In-memory byte captures keep
        // their admission only through the bounded write+flush and therefore do
        // not need a post-response ACK. `image read` is an ordinary terminal-
        // inline projection with no capture job or retained payload slot.
        "image" => sub != Some("read") && !image_bytes,
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
    // `inbox get <id>` returns ONE message body as a length-prefixed byte frame
    // (`OK <nbytes>` + n raw bytes) and `inbox seen <id>` one `OK seen=<id>` status
    // line — neither is the `OK <n>` + rows the bare `inbox` listing answers. The
    // same base-verb/sub-form flip as `image read`, and for the same reason: a
    // client that read `OK seen=42` as a row count hangs waiting for 42 rows that
    // never come. Both sub-forms have their own table row (`inbox get`/`inbox
    // seen`), but `spec()` keys on the verb KEYWORD, so the flip has to be here.
    if verb == "inbox" {
        match sub {
            Some("get") => return Bytes,
            Some("seen") => return Status,
            _ => {}
        }
    }
    // `outbox sent <sid> <id> off=<n>` answers one `OK` status line, not the
    // `OK <nbytes>` + body frame the bare `outbox` drain answers. Same flip, same
    // hazard: a client reading `OK` as a byte count parks on a body that never comes.
    if verb == "outbox" && sub == Some("sent") {
        return Status;
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
    "text", "screen", "cursor", "dims", "blocks", "edges", "grants", "metrics", "privacy",
];

/// One catalog row: the verb name in the [`CATALOG_TEXT_COLUMN`] gutter, then `text`.
fn catalog_row(name: &str, text: &str) -> String {
    format!("{name:<w$} {text}", w = CATALOG_TEXT_COLUMN - 1)
}

/// The SHORT catalog: one `<name padded> <summary>` row per verb, in table order —
/// what the server answers a bare `help` with, bounded by
/// [`SHORT_CATALOG_MAX_BYTES`] in total. [`catalog_lines_full`] is the full form.
/// Both project the one table, so neither can drift from it (they ARE the table).
pub fn catalog_lines() -> impl Iterator<Item = String> {
    VERBS.iter().map(|s| catalog_row(s.name, s.summary))
}

/// The FULL catalog: one `<name padded> <summary detail>` row per verb, in table
/// order — `help --full`, and the `aterm help introspection` manual. Row for row
/// the catalog from before the summary/detail split, except the six rows (`help`,
/// `verbs`, `status`, `turn`, `lease`, `trail`) reworded on purpose so their first
/// sentence fits a summary (the golden test pins every row).
pub fn catalog_lines_full() -> impl Iterator<Item = String> {
    VERBS.iter().map(|s| catalog_row(s.name, &s.help_line()))
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
        // The admission-diagnosis ring streams `OK <n>` + n rows, with no
        // sub-form to change that (`trail 5` is a count, not a mode).
        assert_eq!(framing_of("trail", "trail"), Lines);
        assert_eq!(framing_of("trail", "@s-a trail 5"), Lines);
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
        assert!(!artifact_reply_requires_ack(
            "image",
            "@s-a image --meta --bytes"
        ));
        assert!(!artifact_reply_requires_ack("image", "@s-a image bytes"));
        assert!(!artifact_reply_requires_ack("text", "text"));
        assert!(valid_artifact_ack_nonce("00112233445566778899aabbccddeeff"));
        assert!(!valid_artifact_ack_nonce("artifact"));
    }

    /// `search` matches whole soft-wrapped runs, so two things about a match are
    /// not what a row-at-a-time reader would assume: a hit can run past the grid
    /// width, and `^`/`$` bind to the reader's logical line rather than to a grid
    /// row. A client that knows neither reads the wrong cells and calls find
    /// broken, so the help line has to carry both — this is the seam that keeps
    /// the catalog honest about the semantics the engine actually implements.
    #[test]
    fn search_help_states_where_a_wrapped_match_runs_and_where_anchors_bind() {
        // `help_line()` is summary + detail: the soft-wrap semantics live in the
        // detail half, which `help --full` and `help search` both render.
        let help = spec("search").unwrap().help_line();
        assert!(
            help.contains("SOFT WRAP") && help.contains("col+len"),
            "the help must say a straddling hit's col+len runs past the width"
        );
        assert!(
            help.contains("^") && help.contains("$") && help.contains("LOGICAL"),
            "the help must say regex anchors bind to the logical line"
        );
    }

    #[test]
    fn every_verb_has_a_nonempty_help_line() {
        assert_eq!(catalog_lines().count(), VERBS.len());
        assert_eq!(catalog_lines_full().count(), VERBS.len());
        assert!(VERBS.iter().all(|s| !s.summary.is_empty()));
        assert!(spec("image").unwrap().help_line().contains("image --meta"));
    }

    /// Help that RESTATES THE VERB NAME and adds nothing.
    ///
    /// Ported from clean's help-truth C2, which found this to be the commonest
    /// help failure in a sibling CLI (`--verbose: "Show verbose output"`, 32
    /// instances of that one string). This catalog measures **0 of 95** today,
    /// which is the reason to pin it rather than to skip it: the check costs a
    /// millisecond and the property is one a hurried entry loses first.
    ///
    /// A row fails when, after removing the verb's own words and a list of
    /// filler verbs and articles, at most ONE content word survives — i.e. the
    /// summary told a reader nothing they could not have read off the name.
    #[test]
    fn no_summary_is_a_restatement_of_the_verb_name() {
        const FILLER: &[&str] = &[
            "the", "a", "an", "for", "of", "to", "and", "or", "this", "its", "it", "in", "on",
            "with", "from", "print", "prints", "show", "shows", "display", "displays", "run",
            "runs", "get", "gets", "set", "sets", "output", "command", "current", "aterm", "ctl",
        ];
        let words = |s: &str| -> Vec<String> {
            s.to_lowercase()
                .split(|c: char| !c.is_ascii_alphanumeric())
                .filter(|w| !w.is_empty())
                .map(str::to_string)
                .collect()
        };
        let mut bad: Vec<String> = Vec::new();
        for spec in VERBS {
            let own: std::collections::HashSet<String> = words(spec.name).into_iter().collect();
            let content = words(spec.summary)
                .into_iter()
                .filter(|w| !own.contains(w) && !FILLER.contains(&w.as_str()))
                .count();
            if content <= 1 {
                bad.push(format!("{}: {:?}", spec.name, spec.summary));
            }
        }
        assert!(
            bad.is_empty(),
            "{} catalog summary(ies) restate the verb name and add nothing — a \
             reader who did not know what the verb does still does not:\n  {}",
            bad.len(),
            bad.join("\n  ")
        );
    }

    /// Every repo-rooted path a catalog entry names must exist.
    ///
    /// Ported from clean's help-truth C3. A reader who follows a path out of
    /// help and gets nothing cannot tell whether they typed it wrong or the tool
    /// is lying.
    ///
    /// **This catalog names ZERO repo paths today, so the check is vacuous over
    /// it — and that is stated rather than hidden.** The first draft of this
    /// comment claimed the catalog named one (`docs/AGENT-EXPERIENCE-…`, which
    /// is in a doc comment on line 162, not in any `summary`/`detail`), and the
    /// plant that should have proved the check red PASSED. A gate that reads
    /// nothing reports success, which is the failure mode this whole family of
    /// checks exists to prevent, so the extractor is proved on a synthetic
    /// entry in the same test: if it ever stops finding a planted path, the
    /// test fails whatever the catalog holds.
    ///
    /// The surface that DOES name repo paths is the manual
    /// (`crates/aterm-cli/src/manual.rs`); it has its own check.
    #[test]
    fn every_repo_path_named_in_the_catalog_exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(std::path::Path::parent)
            .expect("crates/aterm-types has a workspace root two levels up");
        const ROOTS: &[&str] = &["crates/", "scripts/", "docs/", "tests/", "data/"];
        let scan = |name: &str, text: &str, missing: &mut Vec<String>| {
            for raw in text.split(|c: char| c.is_whitespace() || c == '`' || c == '"') {
                let tok = raw.trim_matches(|c: char| {
                    !c.is_alphanumeric() && c != '/' && c != '.' && c != '-' && c != '_'
                });
                if ROOTS.iter().any(|r| tok.starts_with(r)) && !root.join(tok).exists() {
                    missing.push(format!("{name}: `{tok}`"));
                }
            }
        };

        // THE EXTRACTOR IS PROVED FIRST. Everything below is vacuous over a
        // catalog that names no paths, and a vacuous check reports success.
        let mut probe = Vec::new();
        scan(
            "synthetic",
            "see `docs/NO-SUCH-FILE-9f3a.md` and crates/aterm-types/src/control_verbs.rs",
            &mut probe,
        );
        assert_eq!(
            probe,
            vec!["synthetic: `docs/NO-SUCH-FILE-9f3a.md`".to_string()],
            "the extractor must find a missing path and pass a real one; if this \
             fails the check below proves nothing about the catalog"
        );

        let mut missing = Vec::new();
        for spec in VERBS {
            scan(spec.name, spec.summary, &mut missing);
            scan(spec.name, spec.detail, &mut missing);
        }
        assert!(
            missing.is_empty(),
            "{} repo-rooted path(s) named in the verb catalog do not exist:\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// The two-tier contract: every summary fits one catalog row, and the whole
    /// short catalog fits the discovery budget — COMPUTED from the table, so a row
    /// that grows past either bound fails here rather than in an agent's context
    /// window. Also the split's hygiene: no summary is padded or doubled-spaced (the
    /// wrap and the re-join both rely on single spaces), and `help_line` is exactly
    /// `summary`, one space, `detail`.
    #[test]
    fn summaries_fit_the_short_catalog_budget() {
        let mut total = 0usize;
        for s in VERBS {
            let n = s.summary.chars().count();
            assert!(
                n <= SUMMARY_MAX_CHARS,
                "{}: summary is {n} chars, over the {SUMMARY_MAX_CHARS} cap: {:?}",
                s.name,
                s.summary
            );
            for (field, text) in [("summary", s.summary), ("detail", s.detail)] {
                assert!(
                    !text.contains("  "),
                    "{}: {field} has a run of spaces",
                    s.name
                );
                assert_eq!(text.trim(), text, "{}: {field} is padded", s.name);
            }
            assert!(
                !s.detail.starts_with(' '),
                "{}: detail starts with a space",
                s.name
            );
            let expect = if s.detail.is_empty() {
                s.summary.to_string()
            } else {
                format!("{} {}", s.summary, s.detail)
            };
            assert_eq!(
                s.help_line(),
                expect,
                "{}: help_line is not summary + ' ' + detail",
                s.name
            );
            total += catalog_row(s.name, s.summary).len() + 1;
        }
        let short: usize = catalog_lines().map(|l| l.len() + 1).sum();
        assert_eq!(short, total);
        assert!(
            short <= SHORT_CATALOG_MAX_BYTES,
            "the short catalog is {short} bytes, over the {SHORT_CATALOG_MAX_BYTES} budget"
        );
        // Non-vacuity: the split is real — the table carries detail somewhere, and the
        // full catalog is far larger than the short one.
        assert!(VERBS.iter().any(|s| !s.detail.is_empty()));
        let full: usize = catalog_lines_full().map(|l| l.len() + 1).sum();
        assert!(full > 2 * short, "full {full} B vs short {short} B");
    }

    /// `entry_lines` is the `help <verb>` body: first row `<name padded> <text>`, every
    /// continuation row indented to the text column, no row wider than the wrap
    /// width unless a single word is, and re-joining the text reproduces `help_line`
    /// byte-for-byte — for EVERY verb, so the wrap is provably lossless and stable.
    #[test]
    fn entry_lines_wrap_every_verb_deterministically_and_losslessly() {
        let gutter = " ".repeat(CATALOG_TEXT_COLUMN);
        let text_width = ENTRY_WRAP_COLUMNS - CATALOG_TEXT_COLUMN;
        for s in VERBS {
            let lines = s.entry_lines();
            assert!(!lines.is_empty());
            let head = format!("{:<28} ", s.name);
            assert!(
                lines[0].starts_with(&head),
                "{}: first row leads with the name",
                s.name
            );
            let mut texts = vec![lines[0][head.len()..].to_string()];
            for l in &lines[1..] {
                assert!(
                    l.starts_with(&gutter),
                    "{}: continuation row not indented",
                    s.name
                );
                assert!(
                    !l[gutter.len()..].starts_with(' '),
                    "{}: doubled indent",
                    s.name
                );
                texts.push(l[gutter.len()..].to_string());
            }
            for l in &lines {
                let chars = l.chars().count();
                let one_word = !l.trim_start().contains(' ');
                assert!(
                    chars <= ENTRY_WRAP_COLUMNS || one_word,
                    "{}: row of {chars} chars breaks the {ENTRY_WRAP_COLUMNS}-column wrap: {l:?}",
                    s.name
                );
            }
            for t in &texts {
                assert!(
                    !t.is_empty() && t.trim() == t,
                    "{}: ragged wrap chunk {t:?}",
                    s.name
                );
                // Greedy: a chunk only ends because the next word would not fit.
                assert!(t.chars().count() <= text_width || !t.contains(' '));
            }
            assert_eq!(texts.join(" "), s.help_line(), "{}: wrap lost text", s.name);
            assert_eq!(
                s.entry_lines(),
                lines,
                "{}: wrap is not deterministic",
                s.name
            );
        }
        // A short verb is exactly its one full-catalog row; a long one wraps.
        let one = spec("lines").unwrap();
        assert_eq!(one.entry_lines(), vec![catalog_row("lines", one.summary)]);
        assert!(spec("image").unwrap().entry_lines().len() > 5);
    }

    /// The §6 terminology rule: the catalog uses "connection" for BOTH network
    /// dials and session fabric, so the connection-grain verbs must say
    /// "session connection" and the dial rows "network-drive connection" —
    /// one help catalog never overloads the bare word.
    #[test]
    fn connection_help_terminology_never_overloads_the_bare_word() {
        for v in ["connect", "disconnect", "flows", "raise", "spawn"] {
            let help = spec(v).unwrap().help_line();
            assert!(
                help.contains("session connection") || help.contains("session-connection"),
                "{v} help must say \"session connection\""
            );
        }
        for v in ["dial-list", "dial-token"] {
            assert!(
                spec(v)
                    .unwrap()
                    .help_line()
                    .contains("network-drive connection"),
                "{v} help must say \"network-drive connection\""
            );
        }
    }

    /// The placement columns are documented where the wire is (F2/F5): `sessions`
    /// spells every `window=` value, says a headless instance is `window=0` (it
    /// owns logical window 0, the one `dims` names) and NOT `none`, and names
    /// the sanitized `detail=`; `status` says why it carries no `window=` (it is
    /// polled, and the window lives on the main thread). Pinned so the help a
    /// reader is handed cannot drift from what the verbs do.
    #[test]
    fn placement_columns_are_documented_on_the_roster_and_not_on_status() {
        let sessions = spec("sessions").expect("sessions is in the table");
        assert!(
            sessions
                .summary
                .ends_with("meta= nonce= window= active= wfocus= detail="),
            "{}",
            sessions.summary
        );
        for phrase in [
            "`nonce=<hex32>` is the session's PUBLIC launch nonce",
            "`window=<id|none|->`",
            "`none` = a session no window holds",
            "`-` = the main thread could not be asked",
            "A `--headless` instance owns one logical window, id 0",
            "`window=0 geometry=headless`",
            "so its sessions read `window=0`, never `none`",
            "`active=<1|0|->`",
            "`wfocus=<1|0|->`",
            "`detail=<pct|->`",
            "never an argument",
            "a COMPOUND command's first word is the shell keyword that opens",
        ] {
            assert!(
                sessions.detail.contains(phrase),
                "sessions detail lacks {phrase:?}"
            );
        }
        let status = spec("status").expect("status is in the table");
        assert!(
            status
                .detail
                .contains("No `window=` here: `status` is polled"),
            "status must say why it carries no window="
        );
        assert!(
            status
                .detail
                .contains("`detail=` is the sanitized RUNNING command"),
            "status documents the populated detail="
        );
        // Honesty (Phase 4): `detail=` is the first WORD, and for a compound
        // command that word is the shell keyword — measured `for i in 1 2 3;
        // do ...; done` → `detail=for`. Both entries must say so, or the help
        // promises a program name the reducer never produces.
        for entry in [sessions, status] {
            assert!(
                entry.detail.contains("`for i in 1 2 3; do ...; done`"),
                "{} must name the measured compound-command reading",
                entry.name
            );
        }
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
                "operator",
                "operator-propose-bin",
                "sessions",
                "who",
                "exits",
                "whoami",
                "grant",
                "revoke",
                "connect",
                "disconnect",
                "flows",
                "raise",
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
            ["version", "update", "help", "verbs", "privacy"],
            "the AnyScopeMeta set (answered pre-scope for any authenticated caller)",
        );

        // The BridgeOnly set. It is EXACTLY the verbs that would let a prompt-injected
        // agent holding Owner scope forge an attested human order into a sibling's
        // inbox (`deliver`, which stamps `from=`/`trust=`), lift a fleet halt locally
        // (`hold`), read every session's outbound traffic (`outbox`), or release a
        // `post --wait` for a message that never left the machine (`outbox sent`).
        // Every in-session client already holds Owner, so a member arriving here
        // without that property is a real widening of the one authority no token
        // unlocks — which is why the set is pinned, not counted.
        let bridge_only: Vec<&str> = VERBS
            .iter()
            .filter(|s| matches!(s.access, Access::BridgeOnly))
            .map(|s| s.name)
            .collect();
        assert_eq!(
            bridge_only,
            ["deliver", "hold", "outbox", "outbox sent"],
            "the BridgeOnly set (only the inherited bridge connection may run these)",
        );

        // The predicates project the same truth, and the three exception sets are
        // pairwise DISJOINT — a verb has exactly one scope gate.
        for v in &owner_only {
            assert!(is_owner_only(v), "{v} is_owner_only");
            assert!(!is_any_scope_meta(v), "{v} not any-scope-meta");
            assert!(!is_bridge_only(v), "{v} not bridge-only");
        }
        for v in &any_meta {
            assert!(is_any_scope_meta(v), "{v} is_any_scope_meta");
            assert!(!is_owner_only(v), "{v} not owner-only");
            assert!(!is_bridge_only(v), "{v} not bridge-only");
        }
        for v in &bridge_only {
            assert!(is_bridge_only(v), "{v} is_bridge_only");
            assert!(!is_owner_only(v), "{v} not owner-only");
            assert!(!is_any_scope_meta(v), "{v} not any-scope-meta");
        }
        // A normal `Scoped` verb is none of them; unknown verbs are none of them.
        assert!(!is_owner_only("text") && !is_any_scope_meta("text") && !is_bridge_only("text"));
        assert!(!is_owner_only("bogus") && !is_any_scope_meta("bogus") && !is_bridge_only("bogus"));
        // The fabric verbs an agent inside the session calls are ORDINARY scoped verbs:
        // classifying `inbox`/`post` bridge-only would leave the agent unable to read
        // its own mail, which is the whole point of the ring.
        for v in ["inbox", "inbox get", "inbox seen", "post"] {
            assert!(
                matches!(
                    spec(v).expect("fabric verb is in the table").access,
                    Access::Scoped
                ),
                "{v} is a scoped verb"
            );
        }
    }

    /// The fabric rows carry exactly the classes §11.2 of the fabric design gives
    /// them, and `framing_of` flips BOTH `inbox` sub-forms. The classes are what the
    /// server gates on and what the client parses with, so a silent change to one of
    /// them is a protocol change with no other alarm.
    #[test]
    fn fabric_verbs_carry_their_designed_classes() {
        for (name, op, framing, target, access) in [
            ("inbox", Read, Lines, Session, Scoped),
            ("inbox get", Read, Bytes, Session, Scoped),
            ("inbox seen", Write, Status, Session, Scoped),
            ("post", Write, Status, Session, Scoped),
            ("deliver", Write, Status, Meta, BridgeOnly),
            ("hold", Write, Status, Meta, BridgeOnly),
            ("outbox", Read, Bytes, Meta, BridgeOnly),
            ("outbox sent", Write, Status, Meta, BridgeOnly),
        ] {
            let s = spec(name).unwrap_or_else(|| panic!("{name} is in the table"));
            assert_eq!(s.op, op, "{name} op-class");
            assert_eq!(s.framing, framing, "{name} framing");
            assert_eq!(s.target, target, "{name} target");
            assert_eq!(s.access, access, "{name} access");
        }
        // The sub-form flip: the base listing is Lines, `get` is a byte body, `seen`
        // is a status line. Selector-aware, like every other sub-form rule.
        assert_eq!(framing_of("inbox", "inbox"), Lines);
        assert_eq!(framing_of("inbox", "inbox 20 since=4 --meta"), Lines);
        assert_eq!(framing_of("inbox", "inbox get 7"), Bytes);
        assert_eq!(framing_of("inbox", "@s-a inbox get 7"), Bytes);
        assert_eq!(framing_of("inbox", "inbox seen 7 handled"), Status);
        assert_eq!(framing_of("inbox", "@s-a inbox seen 7"), Status);
        // `deliver`/`hold` are single status lines with no sub-form to change that.
        assert_eq!(
            framing_of("deliver", "deliver s-a off=9 from=h-x kind=task"),
            Status
        );
        assert_eq!(framing_of("hold", "hold s-a on reason=x"), Status);
        assert_eq!(framing_of("post", "post to=@s-b kind=ask hello"), Status);
        // `outbox` flips the same way `inbox` does: the base drain is a byte frame,
        // the `sent` sub-form a status line. A client that read `OK` as a byte count
        // would park waiting for a body that never comes.
        assert_eq!(framing_of("outbox", "outbox"), Bytes);
        assert_eq!(framing_of("outbox", "outbox 8"), Bytes);
        assert_eq!(framing_of("outbox", "outbox sent s-a 3 off=91"), Status);
    }

    /// `hold` and `deliver` are the fabric's whole safety story, so the catalog an
    /// agent (or a human at `help hold`) reads must state it: which verbs the halt
    /// refuses, which stay answerable, that the physical keyboard is untouched, and
    /// that a dead bridge is itself a halt. Help that omits the exemptions invites a
    /// driver to treat `ERR halted` as fatal and stop escalating.
    #[test]
    fn hold_help_states_the_halt_surface_and_its_exemptions() {
        let hold = spec("hold").expect("hold ships").help_line();
        for phrase in [
            "ERR halted",
            "from ANY scope",
            "operator-propose-bin",
            // `tab close [N]` RETIRES a session, which is the third act §5.3 says
            // a halt refuses — and the one a driver refused `close` substituted.
            "close invoke hwkey pane tab operator-propose-bin",
            "`tab` is, because `tab close [N]` RETIRES a session",
            "`post`, `inbox seen`, `meta set`, `lease` and every read verb stay answerable",
            "physical keyboard is untouched",
            "reason=fabric-lost",
            "must not depend on a killable process staying alive",
        ] {
            assert!(hold.contains(phrase), "hold help lacks {phrase:?}");
        }
        // The bridge-only pair says WHY it is not an Owner verb, in the table that
        // gates it — the reasoning has to live where the classification does.
        let deliver = spec("deliver").expect("deliver ships").help_line();
        assert!(deliver.contains("BRIDGE-ONLY") && deliver.contains("Idempotent on `off=`"));
        assert!(deliver.contains("ERR quota") && deliver.contains("64 unread rows"));
        assert!(hold.contains("Owner scope is what every"));
        // `post` documents the edge refusal, which is a per-handler check no class
        // in this table expresses.
        assert!(
            spec("post")
                .expect("post ships")
                .help_line()
                .contains("REFUSED to an edge-token connection"),
            "post help must state the Edge refusal"
        );
    }

    /// EVERY FABRIC ROW STATES THE BOUND IT IS ACTUALLY HELD TO.
    ///
    /// aterm ships no evidence manifest, so these rows ARE its claims — and the
    /// help an agent reads with `help <verb>` is the only place a driver can learn
    /// a contract. Each assertion below replaced a sentence that promised more
    /// than the code delivered, and each promise had a reachable failure:
    ///
    /// * `inbox get` said "the FULL body" while the bridge had begun delivering
    ///   over-budget bodies TRUNCATED. Nothing recovers the rest — the record is
    ///   on the bus, which the recipient cannot reach — so the answer has to say
    ///   so, and the row has to say that it does.
    /// * `deliver` said "exactly-once at the endpoint" with no qualifier over a
    ///   1024-offset dedup window that ordinary refills reach, and stated its
    ///   quota per `from=` when `from=`'s first half is the sending node's own
    ///   word.
    /// * `post` answered a still-queued message with a bare `ERR fabric
    ///   disconnected`, which reads as "not sent" — and the remedy for "not sent"
    ///   is to send again, into an inbox with no idempotency key.
    /// * `inbox seen` moves the LISTED state as well as the handled watermark,
    ///   which is what releases a sender's quota; the two rows read as if the
    ///   watermarks were independently controlled.
    /// * `send`'s duplicate reply is framed per verb (`OK 0 dup=1` for a
    ///   Lines/Bytes verb), and `turn`'s duplicate carries none of the verdict
    ///   fields its own row promises.
    /// * `outbox` is bounded in bytes across all sessions, not only by `<max>`.
    #[test]
    fn the_fabric_rows_state_the_bounds_they_are_held_to() {
        let help = |verb: &str| {
            spec(verb)
                .unwrap_or_else(|| panic!("{verb} ships"))
                .help_line()
        };

        let get = help("inbox get");
        assert!(
            !get.contains("FULL body"),
            "`inbox get` cannot promise a body the bridge may have cut"
        );
        assert!(get.contains("truncated=1") && get.contains("len=<true-size>"));
        assert!(
            get.contains("NOT recoverable by any verb"),
            "the row must say the rest cannot be fetched, not merely that it is missing"
        );

        let inbox = help("inbox");
        assert!(
            inbox.contains("[truncated=1]"),
            "the row grammar omits the marker"
        );
        assert!(inbox.contains("also LISTS every row at or below"));
        assert!(
            inbox.contains("counts UNHANDLED rows"),
            "`dropped=` counts past the HANDLED watermark, not the listed one"
        );

        assert!(help("inbox seen").contains("RELEASES the sender's per-peer quota"));

        let deliver = help("deliver");
        assert!(
            deliver.contains("over the last 1024 delivered offsets"),
            "the exactly-once claim must carry its window"
        );
        assert!(deliver.contains("reappears as a FRESH row"));
        assert!(
            deliver.contains("ATTESTED PEER") && deliver.contains("cap-forced `<src>`"),
            "the quota is per peer, not per `from=` string"
        );

        assert!(
            help("post").contains("queued=1"),
            "a `--wait` refused for want of a link must not read as `not sent`"
        );

        assert!(help("outbox").contains("BOUNDED IN BYTES"));

        let send = help("send");
        assert!(
            send.contains("`OK dup=1` for a `Status`-framed verb") && send.contains("`OK 0 dup=1`"),
            "the duplicate reply is framed per verb, and `send`'s row is where the \
             key's contract is written"
        );
        assert!(
            help("turn").contains("carries NONE of the verdict fields"),
            "a duplicate `turn` answers a marker, not the verdict line its row promises"
        );
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

    /// The `privacy` row's SHAPE is load-bearing, not cosmetic. `AnyScopeMeta` is
    /// why a `--headless` instance answers it at all (`v()` would have hardcoded
    /// `Access::Scoped`, under which the consent readout is unavailable to exactly
    /// the caller it exists for); `Meta` rejects a selector; `Read` because every
    /// fact it aggregates is one `status` already exposes a session at a time, so
    /// the verb introduces no new authority; `Lines` because it replies `OK <n>`
    /// plus n rows. The help is the wire documentation an agent reads after an
    /// EPERM, so it must also carry the three things that are otherwise guessed
    /// wrong: reading it raises no dialog, per-folder state is `unknown` by
    /// construction, and `unavailable` is a third value, not `false`.
    #[test]
    fn privacy_is_a_headless_answerable_meta_read_that_documents_its_unknowns() {
        let s = spec("privacy").expect("privacy is in the table");
        assert_eq!(
            s.op, Read,
            "privacy is a Read verb (json-capable verbs must be)"
        );
        assert_eq!(s.framing, Lines, "privacy replies `OK <n>` + n lines");
        assert_eq!(
            s.target, Meta,
            "privacy is self-scoped; a selector is rejected"
        );
        assert_eq!(
            s.access, AnyScopeMeta,
            "privacy must be AnyScopeMeta or a headless instance cannot answer it"
        );
        assert!(is_any_scope_meta("privacy") && !is_owner_only("privacy"));
        assert!(
            JSON_CAPABLE_VERBS.contains(&"privacy"),
            "privacy --json is part of the contract"
        );
        assert_eq!(framing_of("privacy", "privacy --json"), Lines);
        assert_eq!(framing_of("privacy", "privacy"), Lines);

        let help = s.help_line();
        for phrase in [
            "no `@<sel>`",
            "`--headless`",
            "raises NO dialog",
            "`unknown` BY CONSTRUCTION",
            "THIRD value",
            "distinct from `off` and from `false`",
            "`fda_scope=unknown`",
            "`sessions_total=` always equals the number of `session` lines",
        ] {
            assert!(help.contains(phrase), "privacy help lacks {phrase:?}");
        }
        // The scope ruling: a grant MITIGATES a class of interruption for the
        // folders it covers. Which services those are is unmeasured, so the help
        // may never say the grant ends prompting.
        assert!(
            help.contains("removes this class of interruption for the folders that grant covers"),
            "the Full Disk Access claim must stay scoped to a class and to covered folders"
        );
        for overclaim in [
            "all prompts",
            "every prompt",
            "no more prompts",
            "never prompt",
            "eliminates",
        ] {
            assert!(
                !help.contains(overclaim),
                "privacy help overclaims what a grant does: {overclaim:?}"
            );
        }
    }

    /// `await consent` is the park-until-the-posture-moves predicate. Two things
    /// have to be IN the help or a caller writes the edge-trigger bug for us: it
    /// arms when the request arrives (so it can never latch on a value that was
    /// already true), and its default timeout is finite precisely because the
    /// dialog it waits behind is not. It also must not claim to see the human's
    /// answer — aterm observes its own posture, nothing more.
    #[test]
    fn await_declares_the_consent_predicate_its_arm_point_and_a_finite_timeout() {
        let s = spec("await").expect("await is in the table");
        assert!(
            s.summary.contains("|consent>"),
            "the grammar row must list the consent token: {:?}",
            s.summary
        );
        let help = s.help_line();
        for phrase in [
            "ARMS when the request arrives",
            "first observed change from THAT baseline",
            "already true when you asked",
            "never against a baseline captured earlier",
            "`timeout=300000`",
            "finite on purpose",
            "an ordinary timeout reply, not an error",
        ] {
            assert!(help.contains(phrase), "await help lacks {phrase:?}");
        }
        assert!(
            help.contains("not that a human answered a dialog"),
            "await must not claim it observes the human's answer"
        );
    }

    /// The FULL catalog is a wire surface (`help --full`, `aterm help introspection`),
    /// pinned byte-for-byte to a GENERATED fixture. The fixture was first captured
    /// from the one-string `help` field before it was split into `summary` + `detail`,
    /// then regenerated ON PURPOSE once the split reworded six rows (`help`, `verbs`,
    /// `status`, `turn`, `lease`, `trail`) whose first sentence ran past the summary
    /// cap; every other row is that capture verbatim, so the pin is the proof that the
    /// split lost nothing anywhere else. Regenerate ONLY after a deliberate wording
    /// change, with the `#[ignore]`d writer below.
    const HELP_CATALOG_FULL_GOLDEN: &str = include_str!("../tests/fixtures/help_catalog_full.txt");

    /// The fixture's exact shape: every full catalog line, `\n`-terminated.
    fn rendered_full_catalog() -> String {
        let mut s = String::new();
        for line in catalog_lines_full() {
            s.push_str(&line);
            s.push('\n');
        }
        s
    }

    #[test]
    fn full_catalog_matches_the_generated_golden() {
        let got = rendered_full_catalog();
        assert!(
            !HELP_CATALOG_FULL_GOLDEN.is_empty(),
            "the golden fixture is empty — it was never generated"
        );
        // Name the first differing line so a failure reads as a diff, not a wall.
        for (i, (g, w)) in got
            .lines()
            .zip(HELP_CATALOG_FULL_GOLDEN.lines())
            .enumerate()
        {
            assert_eq!(
                g,
                w,
                "full catalog line {} drifted from the golden (regenerate on purpose with \
                 `targo --unverified test -p aterm-types --lib -- --ignored regen_help_catalog_golden`)",
                i + 1
            );
        }
        assert_eq!(
            got, HELP_CATALOG_FULL_GOLDEN,
            "full catalog and the golden differ in length"
        );
    }

    /// Writes the golden. Ignored so a routine test run can never rewrite the pin;
    /// run it by name after a DELIBERATE change to a verb's wording.
    #[test]
    #[ignore = "rewrites tests/fixtures/help_catalog_full.txt; run by name on purpose"]
    fn regen_help_catalog_golden() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/help_catalog_full.txt"
        );
        std::fs::write(path, rendered_full_catalog()).expect("write the golden fixture");
    }
}
