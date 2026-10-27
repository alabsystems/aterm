// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-thread parser and main-thread dispatch for the `rewind` verb, the pane history export and the pane task's check.

use aterm_aos::contract::Verdict;
use aterm_aos_pane::trace::PaneCheck;
use aterm_aos_pane::{PaneCode, PaneCounts};
use aterm_buffer::Ticks;
use aterm_control::asciicast::{CastEvent, CastHeader};
use aterm_core::grid::ResizeLaw;
use aterm_core::terminal::Terminal;

use crate::App;
use crate::cast_scrub::CastScrub;
use crate::rewind::{Reply, StepBy, Target};
use crate::temporal::{Export, Exported, NoExport};

/// One parsed `rewind` request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Asked {
    Status,
    Open(Target),
    Step(StepBy),
    /// Where cell `(row, col)` (0-based) last changed at or before the shown instant.
    Changed(u16, u16),
    Text {
        trim: bool,
    },
    Close,
    /// Drop the kept history of this session, or with `all` of every session.
    Forget {
        all: bool,
    },
}

const USAGE: &str = "usage: rewind [status] | open [ago=<secs>|at=<tick>] | step <+-n> [by=sec|cmd] \
                     | changed <row>,<col> | text [trim] | export | check [export] | close \
                     | forget [all]";

/// The directory under the state dir `rewind export` writes into.
const EXPORT_DIR: &str = "rewind";

/// Parse the words after `rewind`, `export` and `check` aside ([`cmd`]).
pub(crate) fn parse(rest: &str) -> Result<Asked, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let usage = || USAGE.to_string();
    let count = |n: &str| {
        n.strip_prefix('+')
            .unwrap_or(n)
            .parse::<i64>()
            .map_err(|_| usage())
    };
    match words.as_slice() {
        [] | ["status"] => Ok(Asked::Status),
        ["open"] => Ok(Asked::Open(Target::Latest)),
        ["open", arg] => {
            let (key, value) = arg.split_once('=').ok_or_else(usage)?;
            let n = value.parse::<u64>().map_err(|_| usage())?;
            match key {
                "ago" => Ok(Asked::Open(Target::Ago(n))),
                "at" => Ok(Asked::Open(Target::At(Ticks(n)))),
                _ => Err(usage()),
            }
        }
        ["step", n] => count(n).map(|n| Asked::Step(StepBy::Slice(n))),
        ["step", n, "by=sec"] => count(n).map(|n| Asked::Step(StepBy::Secs(n))),
        ["step", n, "by=cmd"] => count(n).map(|n| Asked::Step(StepBy::Cmd(n))),
        ["changed", cell] => {
            let (row, col) = cell.split_once(',').ok_or_else(usage)?;
            let row = row.parse::<u16>().map_err(|_| usage())?;
            let col = col.parse::<u16>().map_err(|_| usage())?;
            Ok(Asked::Changed(row, col))
        }
        ["text"] => Ok(Asked::Text { trim: false }),
        ["text", "trim"] => Ok(Asked::Text { trim: true }),
        ["close"] => Ok(Asked::Close),
        ["forget"] => Ok(Asked::Forget { all: false }),
        ["forget", "all"] => Ok(Asked::Forget { all: true }),
        _ => Err(usage()),
    }
}

/// `rewind text`'s reply: `OK <nbytes>[ trimmed=<k>]` and the shown screen, as `temporal` frames it.
pub(crate) fn text_reply(term: &Terminal, lift: u16, trim: bool) -> String {
    let mut body = past_screen_text(term, lift);
    if !trim {
        return format!("OK {}\n{body}", body.len());
    }
    let rows = usize::from(term.rows());
    let sent = crate::control::trimmed_len(body.lines());
    let keep: usize = body.split_inclusive('\n').take(sent).map(str::len).sum();
    body.truncate(keep);
    format!("OK {} trimmed={}\n{body}", body.len(), rows - sent)
}

/// The screen `term` showed before it was drawn `lift` rows higher ([`Terminal::lift_screen`]), a line a row as `text` reads a screen: its newest `lift` history lines, then its screen rows above the lift.
pub(crate) fn past_screen_text(term: &Terminal, lift: u16) -> String {
    use aterm_control::wire::{VisibleTrim, push_visible, visible_row_into};
    let rows = i32::from(term.rows());
    let lift = i32::from(lift).min(rows);
    let mut screen =
        String::with_capacity(usize::from(term.rows()) * (usize::from(term.cols()) + 1));
    for r in -lift..rows - lift {
        match usize::try_from(r) {
            Ok(r) => visible_row_into(term, r, &mut screen),
            Err(_) => push_visible(
                &mut screen,
                &term.get_line_text(r, None).unwrap_or_default(),
                VisibleTrim::Whitespace,
            ),
        }
        screen.push('\n');
    }
    screen
}

/// The verb's control-thread half: `export` and `check` here, the rest asked on the main thread (`Wake::Rewind`).
pub(crate) fn cmd(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    ctx: &crate::SessionCtx,
    session: u64,
    rest: &str,
) -> String {
    match rest.split_whitespace().collect::<Vec<_>>().as_slice() {
        ["export"] => return export_verb(ctx),
        ["check"] => return check_verb(ctx, false),
        ["check", "export"] => return check_verb(ctx, true),
        _ => {}
    }
    let asked = match parse(rest) {
        Ok(asked) => asked,
        Err(why) => return format!("ERR {why}\n"),
    };
    let framed = matches!(asked, Asked::Text { .. });
    match crate::control::control_media::call_main(proxy, |reply| crate::Wake::Rewind {
        session,
        asked,
        reply,
    }) {
        Ok(Ok(reply)) if framed => reply,
        Ok(Ok(line)) => format!("OK {line}\n"),
        Ok(Err(why)) => format!("ERR rewind: {why}\n"),
        Err(e) => format!("ERR {e}\n"),
    }
}

/// `rewind export`: the session's kept history as an asciicast file in `<state dir>/rewind/`, mode 0600.
fn export_verb(ctx: &crate::SessionCtx) -> String {
    let export = export_of(
        &ctx.temporal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    );
    let export = match export {
        Ok(export) => export,
        Err(why) => return format!("ERR rewind: {why}\n"),
    };
    let Some(root) = aterm_types::dirs::state_dir() else {
        return "ERR rewind export: no state directory\n".to_string();
    };
    let (cast, events) = cast_of(&export);
    let name = export_name();
    match write_private(&root, &name, cast.as_bytes(), "rewind export") {
        Ok(path) => format!(
            "OK events={events} bytes={} dropped_input={} lost_resizes={} path={}\n",
            cast.len(),
            export.dropped.0,
            export.dropped.1,
            path.display()
        ),
        Err(e) => format!("ERR rewind export: {e}\n"),
    }
}

/// `rec`'s export, or why there is none: a session that never recorded is told how to turn recording on, and one that records is told what keeps it from replaying.
pub(crate) fn export_of(rec: &crate::temporal::TemporalRecorder) -> Result<Export, &'static str> {
    if rec.total_events() == 0 {
        return Err(crate::rewind::NOT_RECORDING);
    }
    rec.export().map_err(|no| match no {
        NoExport::Broken => BROKE,
        NoExport::NothingReplayable | NoExport::BytesGone => NOTHING_REPLAYS,
    })
}

/// Why `rec` has no chain for `rewind check`, in [`export_of`]'s words.
pub(crate) fn unreplayable(rec: &crate::temporal::TemporalRecorder) -> &'static str {
    if rec.total_events() == 0 {
        crate::rewind::NOT_RECORDING
    } else if rec.spine_watermark().break_open() {
        BROKE
    } else {
        NOTHING_REPLAYS
    }
}

/// Why a recording session replays nothing while output it dropped has no keyframe after it.
const BROKE: &str =
    "the recording broke under a burst of output; it replays again after this pane's next output";

/// Why a recording session with no break open replays nothing.
const NOTHING_REPLAYS: &str = "nothing recorded replays now";

/// `rewind check [export]`: every replayable chain replayed under this build as the pane task and judged by `pane.faithful-replay`, with no live effect; `export` also writes the first failing chain (else the newest) as a pane trace, mode 0600, under the state directory's `rewind/`, for `aos check`.
fn check_verb(ctx: &crate::SessionCtx, export: bool) -> String {
    let chains = {
        let rec = ctx
            .temporal
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match rec.pane_chains().filter(|chains| !chains.is_empty()) {
            Some(chains) => chains,
            None => return format!("ERR rewind: {}\n", unreplayable(&rec)),
        }
    };
    let build = aterm_types::version::build_id();
    let checks: Vec<PaneCheck> = chains
        .iter()
        .map(|chain| chain.check(PaneCode::ThisBuild, &build))
        .collect();
    let k = shown_chain(&checks);
    let mut line = check_line(&checks);
    if export {
        let Some(root) = aterm_types::dirs::state_dir() else {
            return "ERR rewind check export: no state directory\n".to_string();
        };
        let bytes = match chains[k].trace(PaneCode::ThisBuild, &build) {
            Ok(trace) => trace.encode(),
            Err(f) => return format!("ERR rewind check export refused={}: {f}\n", f.word()),
        };
        match write_private(
            &root,
            &fresh_name("pane-trace", "aos"),
            &bytes,
            "rewind check export",
        ) {
            Ok(path) => {
                line += &format!(
                    " exported={k} bytes={} path={}",
                    bytes.len(),
                    path.display()
                );
            }
            Err(e) => return format!("ERR rewind check export: {e}\n"),
        }
    }
    format!("OK {line}\n")
}

/// The chain `rewind check` shows the digests of and `export` writes: the first that failed, else the newest.
fn shown_chain(checks: &[PaneCheck]) -> usize {
    checks
        .iter()
        .position(|c| c.verdict != Verdict::Pass)
        .unwrap_or(checks.len().saturating_sub(1))
}

/// `rewind check`'s answer: the chains, their inputs and matched witnesses and replies summed, the first failing chain's law, index and cursor, and the start, state and observation-chain digests of that chain, else of the newest.
pub(crate) fn check_line(checks: &[PaneCheck]) -> String {
    let mut n = PaneCounts::default();
    let mut replies = 0u64;
    for c in checks {
        n.bytes += c.counts.bytes;
        n.resizes += c.counts.resizes;
        n.witnesses += c.counts.witnesses;
        n.matched += c.counts.matched;
        replies += c.replies_matched;
    }
    let verdict = checks
        .iter()
        .enumerate()
        .find_map(|(k, c)| match &c.verdict {
            Verdict::Pass => None,
            Verdict::Fail { law, cursor, .. } => Some(format!("fail:{law}@{k}:{cursor}")),
        })
        .unwrap_or_else(|| "pass".to_string());
    let digests = checks
        .get(shown_chain(checks))
        .map_or_else(String::new, |c| {
            format!(
                " start={} state={} chain={}",
                c.start.short(),
                c.state_digest.short(),
                c.observation_chain.short()
            )
        });
    format!(
        "chains={} {n} replies={replies} verdict={verdict}{digests}",
        checks.len()
    )
}

/// `export` as asciicast v2 text and its event count: the base keyframe's screen painted at 0 s, then each slice, resize and live keyframe's screen at its seconds after it.
/// Each slice passes [`CastScrub`], which drops the shell-integration nonce and every payload that never paints, and a UTF-8 character split between two slices is written whole in the later one.
pub(crate) fn cast_of(export: &Export) -> (String, usize) {
    let (paint, rows, cols) = painted(&export.base);
    let mut events = vec![CastEvent::Output {
        t: 0.0,
        data: paint,
    }];
    let mut scrub = CastScrub::starting(export.base.parser_ground);
    let mut pending = Vec::new();
    for (us, ev) in &export.events {
        let t = Ticks(*us).0 as f64 / 1_000_000.0;
        match ev {
            Exported::Output(slice) => {
                let mut kept = std::mem::take(&mut pending);
                scrub.feed(slice.bytes(), &mut kept);
                pending = kept.split_off(kept.len() - crate::cast::incomplete_tail_len(&kept));
                if !kept.is_empty() {
                    events.push(CastEvent::Output { t, data: kept });
                }
            }
            Exported::Resize { rows, cols } => events.push(CastEvent::Resize {
                t,
                cols: *cols,
                rows: *rows,
            }),
            Exported::Screen(checkpoint) => {
                pending.clear();
                scrub.restart(checkpoint.parser_ground);
                events.push(CastEvent::Output {
                    t,
                    data: painted(checkpoint).0,
                });
            }
        }
    }
    let mut rest = Vec::new();
    scrub.finish(&mut rest);
    if !rest.is_empty() {
        let t = export
            .events
            .last()
            .map_or(0.0, |(us, _)| Ticks(*us).0 as f64 / 1_000_000.0);
        events.push(CastEvent::Output { t, data: rest });
    }
    let header = CastHeader {
        width: cols,
        height: rows,
        evicted: 1,
        dropped: None,
        resize_undo: Some(true),
        resize_laws: CastHeader::laws_named(Some(ResizeLaw::default().header_value())),
        handoff: None,
    };
    let mut out = String::new();
    aterm_control::asciicast::write_into(&mut out, &header, &events);
    (out, events.len())
}

/// The screens and output state `checkpoint` holds as the bytes that restore them from any state ([`Terminal::serialize_restore_ansi`]), and its size.
fn painted(checkpoint: &aterm_core::terminal::TerminalCheckpoint) -> (Vec<u8>, u16, u16) {
    let term = Terminal::replay_from_checkpoint_bounded(checkpoint, 0);
    let paint = term.serialize_restore_ansi();
    (paint.into_bytes(), term.rows(), term.cols())
}

/// What the save panel of View > Export Pane History… gave.
pub(crate) enum Chosen {
    /// The file the person chose; the panel asked before replacing one.
    Path(std::path::PathBuf),
    /// The person cancelled.
    Cancelled,
    /// No save panel here (off macOS).
    NoPanel,
}

/// A fresh export file name: the wall clock, the process and a per-process count.
pub(crate) fn export_name() -> String {
    fresh_name("pane-history", "cast")
}

/// A fresh file name `<stem>-<ms>-<pid>-<n>.<ext>`: the wall clock, the process and a per-process count.
fn fresh_name(stem: &str, ext: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("{stem}-{ms}-{}-{n}.{ext}", std::process::id())
}

/// Write `bytes` as a new 0600 file `name` in `root`'s `rewind/` folder for `verb`, made 0700 (and made if missing) through the opened folder, so a link there is refused by `verb`'s name before anything changes.
/// # Errors
/// The folder is a link, not a directory or not ours, cannot be made or opened, or `name` exists.
pub(crate) fn write_private(
    root: &std::path::Path,
    name: &str,
    bytes: &[u8],
    verb: &str,
) -> std::io::Result<std::path::PathBuf> {
    let dir = crate::aos_verb::private_child(root, EXPORT_DIR, verb)?;
    dir.write_new_private(std::ffi::OsStr::new(name), bytes)?;
    Ok(dir.path().join(name))
}

/// Write `bytes` as a 0600 file at `path`, the one a save panel returned, leaving its folder's mode as it is; a regular file there (the panel asked first) is replaced.
/// # Errors
/// `path` names no file in a folder, or the folder or a file there cannot be opened or written.
pub(crate) fn write_chosen(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(std::io::Error::other("that is not a file path"));
    };
    crate::pinned_dir::PinnedDir::open_resolved(dir)?.write_private(name, bytes)?;
    Ok(())
}

impl App {
    /// The `Wake::Rewind` handler: `asked` of `session`, answered on `reply` (an open, a step or a search once its fold is shown).
    pub(crate) fn control_rewind(&mut self, session: u64, asked: Asked, reply: Reply) {
        let done = match asked {
            Asked::Status => Some(self.rewind_status(session).map(|line| {
                let worker = if self.rewind_busy(session) {
                    "busy"
                } else {
                    "idle"
                };
                format!("{line} worker={worker}")
            })),
            Asked::Open(target) => self
                .rewind_open(session, target, Some(reply.clone()))
                .err()
                .map(Err),
            Asked::Step(by) => self
                .rewind_step(session, by, Some(reply.clone()))
                .err()
                .map(Err),
            Asked::Changed(row, col) => self
                .rewind_changed_for(session, Some((row, col)), Some(reply.clone()))
                .err()
                .map(Err),
            Asked::Text { trim } => Some(self.rewind_shown(session).map(|term| {
                let lift = self.pool.rewind(session).map_or(0, |v| v.lift);
                text_reply(&crate::term_lock(&term), lift, trim)
            })),
            Asked::Close => {
                self.rewind_close(session);
                Some(self.rewind_status(session))
            }
            Asked::Forget { all: false } => Some(
                self.forget_history(session)
                    .map(|()| "forgot=1".to_string()),
            ),
            Asked::Forget { all: true } => {
                Some(Ok(format!("forgot={}", self.forget_all_history())))
            }
        };
        if let Some(done) = done {
            let _ = reply.send(done);
        }
    }

    /// View > Export Pane History… on window `wid`: the focused pane's history, written only where a save panel says.
    pub(crate) fn export_pane_history(&mut self, wid: crate::WindowId) {
        let Some(session) = self.front_terminal(wid).map(|t| t.session) else {
            return;
        };
        let export = self.pool.get(session).map(|s| {
            export_of(
                &s.ctx
                    .temporal
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner),
            )
        });
        let export = match export {
            Some(Ok(export)) => export,
            Some(Err(why)) => {
                self.post_message(crate::message_reporters::pane_history_export_failed(why));
                return;
            }
            None => return,
        };
        let chosen = if cfg!(target_os = "macos") {
            crate::menu::choose_save_path(
                "Export Pane History",
                "Export",
                &format!("{}.cast", export_name().trim_end_matches(".cast")),
            )
            .map_or(Chosen::Cancelled, Chosen::Path)
        } else {
            Chosen::NoPanel
        };
        self.export_pane_history_to(&export, chosen);
    }

    /// Write `export` where the save panel said, write nothing when it was cancelled, and say `rewind export` is the way where there is no panel.
    pub(crate) fn export_pane_history_to(&mut self, export: &Export, chosen: Chosen) {
        let path = match chosen {
            Chosen::Path(path) => path,
            Chosen::Cancelled => return,
            Chosen::NoPanel => {
                self.post_message(crate::message_reporters::pane_history_export_has_no_panel());
                return;
            }
        };
        let (cast, _) = cast_of(export);
        if let Err(e) = write_chosen(&path, cast.as_bytes()) {
            self.post_message(crate::message_reporters::pane_history_export_failed(
                &format!("cannot write {}: {e}", path.display()),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rewind_verb_parses_its_forms_and_refuses_the_rest() {
        for (rest, want) in [
            ("", Asked::Status),
            (" status ", Asked::Status),
            ("open", Asked::Open(Target::Latest)),
            ("open ago=42", Asked::Open(Target::Ago(42))),
            ("open at=1200", Asked::Open(Target::At(Ticks(1200)))),
            ("step -1", Asked::Step(StepBy::Slice(-1))),
            ("step +3", Asked::Step(StepBy::Slice(3))),
            ("step 2", Asked::Step(StepBy::Slice(2))),
            ("step -5 by=sec", Asked::Step(StepBy::Secs(-5))),
            ("step +1 by=cmd", Asked::Step(StepBy::Cmd(1))),
            ("changed 17,41", Asked::Changed(17, 41)),
            ("text", Asked::Text { trim: false }),
            ("text trim", Asked::Text { trim: true }),
            ("close", Asked::Close),
            ("forget", Asked::Forget { all: false }),
            ("forget all", Asked::Forget { all: true }),
        ] {
            assert_eq!(parse(rest), Ok(want), "{rest:?}");
        }
        for rest in [
            "open ago=",
            "open ago=-1",
            "open when=3",
            "open ago=1 at=2",
            "step",
            "step x",
            "step 1 by=min",
            "step by=sec",
            "changed",
            "changed 3",
            "changed -1,2",
            "changed 1,2,3",
            "text all",
            "close now",
            "forget now",
            "forget all now",
            "export now",
            "rewind",
            "status extra",
        ] {
            assert_eq!(parse(rest), Err(USAGE.to_string()), "{rest:?}");
        }
    }
}
