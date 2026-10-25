// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Control-thread parser and main-thread dispatch for the `rewind` verb, and the pane history export.

use aterm_buffer::Ticks;
use aterm_control::asciicast::{CastEvent, CastHeader};
use aterm_core::grid::ResizeLaw;
use aterm_core::terminal::Terminal;

use crate::App;
use crate::rewind::{Reply, StepBy, Target};
use crate::temporal::{Export, Exported};

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
}

const USAGE: &str = "usage: rewind [status] | open [ago=<secs>|at=<tick>] | step <+-n> [by=sec|cmd] \
                     | changed <row>,<col> | text [trim] | export | close";

/// The directory under the state dir `rewind export` writes into.
const EXPORT_DIR: &str = "rewind";

/// Parse the words after `rewind`, `export` aside ([`cmd`]).
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
        _ => Err(usage()),
    }
}

/// `rewind text`'s reply: `OK <nbytes>[ trimmed=<k>]` and the shown screen, as `temporal` frames it.
pub(crate) fn text_reply(term: &Terminal, trim: bool) -> String {
    let mut body = crate::control::screen_text(term);
    if !trim {
        return format!("OK {}\n{body}", body.len());
    }
    let rows = usize::from(term.rows());
    let sent = crate::control::trimmed_len(body.lines());
    let keep: usize = body.split_inclusive('\n').take(sent).map(str::len).sum();
    body.truncate(keep);
    format!("OK {} trimmed={}\n{body}", body.len(), rows - sent)
}

/// The verb's control-thread half: `export` here, the rest asked on the main thread (`Wake::Rewind`).
pub(crate) fn cmd(
    proxy: &aterm_window::event_loop::EventLoopProxy<crate::Wake>,
    ctx: &crate::SessionCtx,
    session: u64,
    rest: &str,
) -> String {
    if rest.split_whitespace().eq(["export"]) {
        return export_verb(ctx);
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
    let Some(export) = ctx
        .temporal
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .export()
    else {
        return format!("ERR rewind: {NOTHING_TO_EXPORT}\n");
    };
    let Some(root) = aterm_types::dirs::state_dir() else {
        return "ERR rewind export: no state directory\n".to_string();
    };
    let (cast, events) = cast_of(&export);
    let name = export_name();
    match write_private(&root.join(EXPORT_DIR), &name, cast.as_bytes()) {
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

/// Why there is nothing to export.
pub(crate) const NOTHING_TO_EXPORT: &str =
    "this session keeps no replayable history (Settings > Terminal > Rewind)";

/// `export` as asciicast v2 text and its event count: the base keyframe's screen painted at 0 s, then each slice, resize and live keyframe's screen at its seconds after it.
pub(crate) fn cast_of(export: &Export) -> (String, usize) {
    let (paint, rows, cols) = painted(&export.base);
    let mut events = vec![CastEvent::Output {
        t: 0.0,
        data: paint,
    }];
    for (us, ev) in &export.events {
        let t = Ticks(*us).0 as f64 / 1_000_000.0;
        events.push(match ev {
            Exported::Output(slice) => CastEvent::Output {
                t,
                data: slice.bytes().to_vec(),
            },
            Exported::Resize { rows, cols } => CastEvent::Resize {
                t,
                cols: *cols,
                rows: *rows,
            },
            Exported::Screen(checkpoint) => CastEvent::Output {
                t,
                data: painted(checkpoint).0,
            },
        });
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

/// The screen `checkpoint` holds as the bytes that paint it, and its size.
fn painted(checkpoint: &aterm_core::terminal::TerminalCheckpoint) -> (Vec<u8>, u16, u16) {
    let term = Terminal::replay_from_checkpoint_bounded(checkpoint, 0);
    let paint = term.serialize_ansi(usize::from(term.rows()), Some(0));
    (paint.into_bytes(), term.rows(), term.cols())
}

/// A fresh export file name: the wall clock, the process and a per-process count.
pub(crate) fn export_name() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    format!("pane-history-{ms}-{}-{n}.cast", std::process::id())
}

/// Write `bytes` as a new 0600 file `name` in `dir`, made 0700 if missing.
/// # Errors
/// The directory cannot be made private or pinned, or `name` exists.
pub(crate) fn write_private(
    dir: &std::path::Path,
    name: &str,
    bytes: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    aterm_types::fs_restricted::ensure_private_dir(dir)?;
    let pinned = crate::pinned_dir::PinnedDir::open_resolved(dir)?;
    pinned.write_new_private(std::ffi::OsStr::new(name), bytes)?;
    Ok(pinned.path().join(name))
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
            Asked::Status => Some(self.rewind_status(session)),
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
            Asked::Text { trim } => Some(
                self.rewind_shown(session)
                    .map(|term| text_reply(&crate::term_lock(&term), trim)),
            ),
            Asked::Close => {
                self.rewind_close(session);
                Some(self.rewind_status(session))
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
        let export = self.pool.get(session).and_then(|s| {
            s.ctx
                .temporal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .export()
        });
        let Some(export) = export else {
            self.post_message(crate::message_reporters::rewind_failed(NOTHING_TO_EXPORT));
            return;
        };
        let Some(path) = crate::menu::choose_save_path(
            "Export Pane History",
            "Export",
            &format!("{}.cast", export_name().trim_end_matches(".cast")),
        ) else {
            return;
        };
        let (cast, _) = cast_of(&export);
        if let Err(e) = write_chosen(&path, cast.as_bytes()) {
            self.post_message(crate::message_reporters::rewind_failed(&format!(
                "cannot write {}: {e}",
                path.display()
            )));
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
            "export now",
            "rewind",
            "status extra",
        ] {
            assert_eq!(parse(rest), Err(USAGE.to_string()), "{rest:?}");
        }
    }
}
