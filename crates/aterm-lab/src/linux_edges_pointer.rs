// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Independent client placement and native pointer arrival for the Sway edge rows.

use std::collections::BTreeMap;

use super::NativeGeometry;

#[derive(serde::Deserialize)]
struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(serde::Deserialize)]
struct Node {
    app_id: Option<String>,
    pid: Option<u32>,
    visible: Option<bool>,
    rect: Option<Rect>,
    window_rect: Option<Rect>,
    #[serde(default)]
    nodes: Vec<Node>,
    #[serde(default)]
    floating_nodes: Vec<Node>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Placement {
    Ready((i32, i32)),
    Pending(String),
}

/// Sway's `rect` excludes its titlebar; `window_rect` locates the client inside it.
/// The lane uses scale one, and aterm's xdg window geometry is its complete surface.
pub(super) fn client_origin(
    tree: &str,
    pid: u32,
    geometry: NativeGeometry,
) -> Result<Placement, String> {
    let root: Node =
        aterm_json::from_str(tree).map_err(|error| format!("Sway's tree is unread: {error}"))?;
    let mut pending = vec![&root];
    let mut found = None;
    while let Some(node) = pending.pop() {
        pending.extend(&node.nodes);
        pending.extend(&node.floating_nodes);
        if node.app_id.as_deref() != Some("aterm") || node.pid != Some(pid) {
            continue;
        }
        if found.is_some() {
            return Err(format!(
                "Sway shows more than one aterm client for pid {pid}"
            ));
        }
        found = Some(node);
    }
    let Some(node) = found else {
        return Ok(Placement::Pending(format!("no Sway client for pid {pid}")));
    };
    let rect = node.rect.as_ref().ok_or("Sway's client has no rect")?;
    let window = node
        .window_rect
        .as_ref()
        .ok_or("Sway's client has no window_rect")?;
    let visible = node.visible.ok_or("Sway's client has no visibility")?;
    if geometry.scale != 1.0
        || rect.width < 0
        || rect.height < 0
        || window.x < 0
        || window.y < 0
        || window.width < 0
        || window.height < 0
        || i64::from(window.x) + i64::from(window.width) > i64::from(rect.width)
        || i64::from(window.y) + i64::from(window.height) > i64::from(rect.height)
    {
        return Err(format!(
            "invalid Sway geometry for pid {pid}: rect {}x{}, window {}x{}+{}+{}, scale {}",
            rect.width,
            rect.height,
            window.width,
            window.height,
            window.x,
            window.y,
            geometry.scale
        ));
    }
    if !visible || (window.width, window.height) != (geometry.width, geometry.height) {
        return Ok(Placement::Pending(format!(
            "pid {pid}: Sway visible={visible} window {}x{}, GUI surface {}x{}",
            window.width, window.height, geometry.width, geometry.height
        )));
    }
    Ok(Placement::Ready((
        rect.x
            .checked_add(window.x)
            .ok_or("Sway client x overflow")?,
        rect.y
            .checked_add(window.y)
            .ok_or("Sway client y overflow")?,
    )))
}

/// A terminal cell centre from an immutable dims reply, without moving a pointer.
pub(super) fn grid_cell(dims: &str, row: i32, col: i32) -> Result<(i32, i32), String> {
    let geometry = NativeGeometry::parse(dims)?;
    let number = |key| super::dimension(dims, key);
    let (cw, ch, width, height, pad, bottom) = (
        number("cell_w")?,
        number("cell_h")?,
        number("grid_w")?,
        number("grid_h")?,
        number("pad")?,
        number("pad_bottom")?,
    );
    if cw <= 0
        || ch <= 0
        || width <= 0
        || height <= 0
        || pad < 0
        || bottom < 0
        || !(0..width / cw).contains(&col)
        || !(0..height / ch).contains(&row)
        || i64::from(pad) + i64::from(width) > i64::from(geometry.frame_width)
        || i64::from(bottom) + i64::from(height) > i64::from(geometry.frame_height)
    {
        return Err(format!("cell {row},{col} is outside the dims grid: {dims}"));
    }
    geometry.frame_point(
        i64::from(pad) + i64::from(col) * i64::from(cw) + i64::from(cw / 2),
        i64::from(geometry.frame_height) - i64::from(bottom) - i64::from(height)
            + i64::from(row) * i64::from(ch)
            + i64::from(ch / 2),
    )
}

/// The first visible tab-strip row, translated from frame to native window pixels.
/// Unlike terminal padding, this region changes a text hover to the default cursor.
pub(super) fn strip_point(dims: &str) -> Result<(i32, i32), String> {
    let geometry = NativeGeometry::parse(dims)?;
    let number = |key| super::dimension(dims, key);
    let (rows, pad, top, head, cw, ch) = (
        number("tab_rows")?,
        number("pad")?,
        number("pad_top")?,
        number("head")?,
        number("cell_w")?,
        number("cell_h")?,
    );
    if rows <= 0
        || pad < 0
        || top < 0
        || head < 0
        || cw <= 0
        || ch <= 0
        || i64::from(top) + i64::from(head) + i64::from(rows) * i64::from(ch)
            > i64::from(geometry.frame_height)
    {
        return Err(format!("dims has no visible tab-strip row: {dims}"));
    }
    geometry.frame_point(
        i64::from(pad) + i64::from(cw / 2),
        i64::from(top) + i64::from(head) + i64::from(ch / 2),
    )
}

#[derive(Clone, Copy)]
pub(super) enum Cursor {
    Any,
    Shape(usize),
    Surface,
}

fn message(line: &str) -> Option<(bool, &str, &str, Vec<&str>)> {
    let (outgoing, line) = line
        .split_once("-> ")
        .map_or((false, line), |(_, request)| (true, request));
    let (before, args) = line.split_once('(')?;
    let (object, method) = before.split_whitespace().last()?.split_once('.')?;
    let args = args.strip_suffix(')')?.split(',').map(str::trim).collect();
    Some((outgoing, object, method, args))
}

fn coordinate(text: &str) -> Option<f64> {
    let value: f64 = text.parse().ok()?;
    value.is_finite().then_some(value)
}

fn object_id(text: &str) -> Option<u32> {
    text.rsplit_once('@')?.1.parse().ok()
}

struct Pointer {
    at: (f64, f64),
    serial: u32,
    fresh: bool,
    motion: bool,
    framed: bool,
    replay: bool,
    consumed: bool,
}

/// A response after this window's native motion follows GUI event processing.
/// Enter alone is insufficient: the window backend replays its selected cursor
/// before the GUI receives CursorMoved. The fixture has one pointer; reserve its
/// first same-serial response for that replay, even when raw motion and frame
/// logged before the backend drained the batch. A legacy theme that supplies no
/// replay cannot establish an acknowledgment by its absence. Only a later cursor
/// response after framed in-surface motion can acknowledge GUI event processing.
pub(super) fn handled(log: &str, want: (i32, i32), cursor: Cursor) -> bool {
    observed(log, want, Some(cursor), 0)
}

/// A new native arrival after this fixture's append-only wire checkpoint. An
/// already recorded position or a later frame alone cannot release the helper.
pub(super) fn arrived(log: &str, checkpoint: &str, want: (i32, i32)) -> Result<bool, String> {
    if !log.starts_with(checkpoint) {
        return Err("the native pointer wire log changed before warmup arrived".into());
    }
    Ok(observed(log, want, None, checkpoint.len()))
}

fn observed(log: &str, want: (i32, i32), cursor: Option<Cursor>, after: usize) -> bool {
    let want = (f64::from(want.0), f64::from(want.1));
    let Some(surface) = super::window_surface(log) else {
        return false;
    };
    let mut pointers = BTreeMap::<&str, Pointer>::new();
    let mut devices = BTreeMap::<u32, &str>::new();
    let mut last = None;
    let consumed = cursor.is_some();
    let cursor = cursor.unwrap_or(Cursor::Any);
    let mut offset = 0;
    for line in log.split_inclusive('\n') {
        let fresh = offset >= after;
        offset += line.len();
        let Some(line) = line.strip_suffix('\n') else {
            continue;
        };
        let Some((outgoing, object, method, args)) = message(line) else {
            continue;
        };
        if !outgoing && object.starts_with("wl_pointer@") {
            let at = |x: usize, y: usize| {
                Some((
                    coordinate(args.get(x).copied()?)?,
                    coordinate(args.get(y).copied()?)?,
                ))
            };
            match method {
                "enter" if args.get(1) == Some(&surface.as_str()) => {
                    if let (Some(px), Some(serial)) = (
                        at(2, 3),
                        args.first().and_then(|serial| serial.parse().ok()),
                    ) {
                        pointers.insert(
                            object,
                            Pointer {
                                at: px,
                                serial,
                                fresh,
                                motion: false,
                                framed: false,
                                replay: true,
                                consumed: false,
                            },
                        );
                        last = Some(object);
                    } else {
                        pointers.remove(object);
                    }
                }
                "motion" if pointers.contains_key(object) => {
                    if let Some(px) = at(1, 2) {
                        if let Some(pointer) = pointers.get_mut(object) {
                            pointer.at = px;
                            pointer.fresh = fresh;
                            pointer.motion = true;
                            pointer.framed = false;
                            pointer.consumed = false;
                        }
                        last = Some(object);
                    } else {
                        pointers.remove(object);
                    }
                }
                "enter" | "leave" => {
                    pointers.remove(object);
                }
                "frame" => {
                    if let Some(pointer) = pointers.get_mut(object) {
                        pointer.framed = true;
                    }
                }
                _ => {}
            }
        } else if outgoing {
            if object.starts_with("wp_cursor_shape_manager_v1@") && method == "get_pointer" {
                if let (Some(device), Some(pointer)) = (args.first(), args.get(1))
                    && let Some(device) = device.strip_prefix("new id ")
                    && let Some(device) = object_id(device)
                {
                    devices.insert(device, pointer);
                }
            }
            let (response, serial, matches) = if object.starts_with("wp_cursor_shape_device_v1@")
                && method == "set_shape"
                && let Some(serial) = args.first().and_then(|serial| serial.parse::<u32>().ok())
                && let Some(shape) = args.get(1).and_then(|shape| shape.parse::<usize>().ok())
            {
                (
                    object_id(object).and_then(|id| devices.get(&id).copied()),
                    serial,
                    match cursor {
                        Cursor::Any => true,
                        Cursor::Shape(expected) => expected == shape,
                        Cursor::Surface => false,
                    },
                )
            } else if object.starts_with("wl_pointer@")
                && method == "set_cursor"
                && let Some(serial) = args.first().and_then(|serial| serial.parse::<u32>().ok())
                && args
                    .get(1)
                    .is_some_and(|surface| surface.starts_with("wl_surface@"))
            {
                (
                    Some(object),
                    serial,
                    matches!(cursor, Cursor::Any | Cursor::Surface),
                )
            } else {
                (None, 0, false)
            };
            if let Some(pointer) = response
                && let Some(pointer) = pointers.get_mut(pointer)
                && pointer.framed
                && pointer.serial == serial
            {
                if pointer.replay {
                    pointer.replay = false;
                } else if pointer.motion && pointer.at == want && matches {
                    pointer.consumed = true;
                }
            }
        }
    }
    last.and_then(|pointer| pointers.get(pointer))
        .is_some_and(|pointer| {
            pointer.at == want
                && pointer.fresh
                && pointer.framed
                && (!consumed || (pointer.motion && pointer.consumed))
        })
}

#[derive(Clone, Copy)]
pub(super) struct Work {
    birth: u64,
    counters: [u64; 7],
}

pub(super) enum Reading {
    Ended,
    Unknown(Option<u64>),
    Work(Work),
}

/// `cat /proc/PID/stat /proc/PID/io /proc/PID/stat`, bracketing I/O by identity.
pub(super) fn reading(pid: u32, text: &str) -> Reading {
    use aterm_sysprobe::jobs::linux::{StatLine, parse_io, parse_stat};

    fn stat(pid: u32, text: &str) -> Option<StatLine> {
        if text.split_once('(')?.0.trim().parse::<u32>().ok()? != pid {
            return None;
        }
        parse_stat(text)
    }
    let lines: Vec<_> = text.lines().collect();
    let Some(before) = lines.first().and_then(|line| stat(pid, line)) else {
        return Reading::Unknown(None);
    };
    if before.exiting() || matches!(before.state, b'Z' | b'X' | b'x') {
        return Reading::Ended;
    }
    let Some(after) = lines.last().and_then(|line| stat(pid, line)) else {
        return Reading::Unknown(Some(before.start));
    };
    if after.exiting() || matches!(after.state, b'Z' | b'X' | b'x') {
        return Reading::Ended;
    }
    if before.start != after.start {
        return Reading::Unknown(Some(after.start));
    }
    if lines.len() < 3 {
        return Reading::Unknown(Some(before.start));
    }
    let io = parse_io(&lines[1..lines.len() - 1].join("\n"));
    match (io.wchar, io.syscr, io.syscw, io.read_bytes, io.write_bytes) {
        (Some(wchar), Some(syscr), Some(syscw), Some(read), Some(write)) => Reading::Work(Work {
            birth: after.start,
            counters: [after.utime, after.stime, wchar, syscr, syscw, read, write],
        }),
        _ => Reading::Unknown(Some(after.start)),
    }
}

pub(super) struct Progress {
    owners: Vec<(u32, Option<u64>, Option<Work>)>,
    generation: u64,
}

impl Progress {
    pub(super) fn new(pids: &[u32]) -> Self {
        Self {
            owners: pids.iter().map(|&pid| (pid, None, None)).collect(),
            generation: 0,
        }
    }

    /// Refresh the process samples after querying the owners. Their replies do not
    /// count as independent work done while waiting for a configure to take effect.
    pub(super) fn after_probe(&mut self, read: impl FnMut(u32) -> Reading) -> Result<(), String> {
        let generation = self.generation;
        self.observe(read)?;
        self.generation = generation;
        Ok(())
    }

    pub(super) fn observe(
        &mut self,
        mut read: impl FnMut(u32) -> Reading,
    ) -> Result<Option<u64>, String> {
        let (mut comparable, mut grew) = (true, false);
        for (pid, original, previous) in &mut self.owners {
            let (birth, work) = match read(*pid) {
                Reading::Ended => return Err(format!("native pointer owner {pid} ended")),
                Reading::Unknown(birth) => (birth, None),
                Reading::Work(work) => (Some(work.birth), Some(work)),
            };
            if let Some(birth) = birth {
                if original.is_some_and(|original| original != birth) {
                    return Err(format!("native pointer owner {pid} changed identity"));
                }
                *original = Some(birth);
            }
            match (*previous, work) {
                (Some(before), Some(after))
                    if after
                        .counters
                        .iter()
                        .zip(before.counters)
                        .all(|(&new, old)| new >= old) =>
                {
                    grew |= before.counters != after.counters;
                }
                _ => comparable = false,
            }
            *previous = work;
        }
        if comparable && grew {
            self.generation = self.generation.saturating_add(1);
        }
        Ok(comparable.then_some(self.generation))
    }
}

#[cfg(test)]
#[path = "linux_edges_pointer_tests.rs"]
mod tests;
