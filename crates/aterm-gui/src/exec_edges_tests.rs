// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`ExecEdges`] tests over a scripted process watch and scripted images.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use aterm_uds::procwatch::{ProcEvent, ProcScript, ProcWatch};
use atpkg::image::{ImageBuild, ImageSource, ProgramTag};

use super::ExecEdges;
use crate::command_ran::{ExecNames, Ran, RanFeed, Steering, Window};
use crate::foreground_handback::FgCutter;
use crate::holder_image::{EdgeKind, HolderImage};

static IMAGES: Mutex<Option<HashMap<i32, HolderImage>>> = Mutex::new(None);
static READS: Mutex<Option<HashMap<i32, Vec<Option<HolderImage>>>>> = Mutex::new(None);

fn scripted_read(pgid: i32, last: Option<&HolderImage>) -> HolderImage {
    READS
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .entry(pgid)
        .or_default()
        .push(last.cloned());
    IMAGES
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&pgid).cloned())
        .unwrap_or_else(|| HolderImage::unread(pgid))
}

fn reads_of(pgid: i32) -> Vec<Option<HolderImage>> {
    READS
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(&pgid).cloned())
        .unwrap_or_default()
}

fn set_image(image: HolderImage) {
    IMAGES
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(image.pgid, image);
}

fn image(pgid: i32, exe: &str, build: ImageBuild) -> HolderImage {
    HolderImage::of(pgid, Some(7), Some(PathBuf::from(exe)), build, 1)
}

fn shell(pgid: i32) -> HolderImage {
    image(
        pgid,
        "/bin/zsh",
        ImageBuild::unknown(ProgramTag::new("zsh")),
    )
}

fn tool(pgid: i32) -> HolderImage {
    image(
        pgid,
        "/usr/bin/true",
        ImageBuild::unknown(ProgramTag::new("true")),
    )
}

fn claude(pgid: i32) -> HolderImage {
    image(
        pgid,
        "/p/pkg/store/claude/1000002000001000304/bin/claude",
        ImageBuild {
            program: ProgramTag::new("claude"),
            build: 1_000_002_000_001_000_304,
            source: ImageSource::Store,
        },
    )
}

fn launcher(pgid: i32) -> HolderImage {
    image(
        pgid,
        "/bin/sh",
        ImageBuild {
            program: ProgramTag::new("claude"),
            build: 0,
            source: ImageSource::Launcher,
        },
    )
}

fn no_ran(shell_pid: i32) -> RanFeed {
    RanFeed::new(std::sync::Arc::default(), shell_pid)
}

fn rig(shell_pid: i32, seed: i32, held: HolderImage) -> (ExecEdges, ProcScript, FgCutter) {
    let (watch, script) = ProcWatch::scripted(shell_pid).expect("pipe");
    let mut x = ExecEdges::new(watch, scripted_read, seed, no_ran(shell_pid));
    set_image(held.clone());
    x.held = Some(std::sync::Arc::new(held));
    (x, script, FgCutter::new(seed))
}

fn exec(pid: i32, late: bool) -> ProcEvent {
    ProcEvent::Exec {
        pid,
        pgid: pid,
        late,
    }
}

#[test]
fn a_drawer_launch_is_cut_at_each_exec_and_a_tool_folds() {
    const SH: i32 = 51_000;
    const JOB: i32 = 51_001;
    const TOOL: i32 = 51_002;
    let (mut x, script, mut c) = rig(SH, SH, shell(SH));
    set_image(tool(TOOL));
    script.post(&[ProcEvent::Fork { child: TOOL }, exec(TOOL, false)]);
    c.begin();
    assert!(!x.heard(&mut c, || TOOL, 0));
    assert!(c.take_edge().is_none() && x.take_image().is_none());

    set_image(launcher(JOB));
    script.post(&[ProcEvent::Fork { child: JOB }, exec(JOB, false)]);
    c.begin();
    assert!(!c.sample(SH, 40));
    assert!(x.heard(&mut c, || JOB, 40));
    let edge = c.finish(|| JOB, 40).expect("the launcher's edge");
    assert_eq!((edge.kind, edge.from, edge.to), (EdgeKind::Group, SH, JOB));
    assert_eq!(x.take_image().map(|i| i.launcher_for.is_some()), Some(true));

    set_image(claude(JOB));
    c.begin();
    assert!(!c.sample(JOB, 100));
    assert!(!c.sample(JOB, 300));
    script.post(&[exec(JOB, false)]);
    assert!(x.heard(&mut c, || JOB, 300));
    let edge = c.finish(|| JOB, 300).expect("the drawer's edge");
    assert_eq!(
        (edge.kind, edge.at, edge.lay, edge.from, edge.to),
        (EdgeKind::Exec, 100, 300, JOB, JOB)
    );
    assert!(x.take_image().is_some_and(|i| i.drawer.is_some()));

    c.begin();
    script.post(&[ProcEvent::Exec {
        pid: JOB + 100,
        pgid: JOB,
        late: false,
    }]);
    assert!(!x.heard(&mut c, || JOB, 64));
    assert!(c.take_edge().is_none());
}

#[test]
fn a_late_exec_that_restates_the_held_image_folds() {
    const SH: i32 = 52_000;
    const JOB: i32 = 52_001;
    let (mut x, script, mut c) = rig(SH, JOB, claude(JOB));
    c.begin();
    script.post(&[exec(JOB, true)]);
    assert!(!x.heard(&mut c, || JOB, 0), "the same image, late: no edge");
    script.post(&[exec(JOB, false)]);
    assert!(x.heard(&mut c, || JOB, 0));
    assert_eq!(c.take_edge().map(|e| e.kind), Some(EdgeKind::Exec));
    set_image(launcher(JOB));
    c.begin();
    script.post(&[exec(JOB, true)]);
    assert!(x.heard(&mut c, || JOB, 0));
    assert!(c.take_edge().is_some());
}

#[test]
fn an_exec_heard_after_the_batch_edge_waits_for_a_byte_free_one() {
    const SH: i32 = 53_000;
    const JOB: i32 = 53_001;
    let (mut x, script, mut c) = rig(SH, SH, shell(SH));
    set_image(launcher(JOB));
    c.begin();
    assert!(c.sample(JOB, 10));
    script.post(&[exec(JOB, false)]);
    assert!(x.heard(&mut c, || JOB, 10), "the batch still delivers");
    assert!(x.has_deferred());
    let edge = c.finish(|| JOB, 10).expect("the sampled edge");
    assert_eq!(edge.kind, EdgeKind::Group);
    set_image(claude(JOB));
    c.begin();
    assert!(x.replay(&mut c, || JOB));
    let edge = c.take_edge().expect("the byte-free edge");
    assert_eq!(
        (edge.kind, edge.at, edge.lay, edge.to),
        (EdgeKind::Exec, 0, 0, JOB)
    );
    assert!(!x.has_deferred());
}

#[test]
fn a_sampled_edge_absorbs_its_leaders_exec() {
    const SH: i32 = 54_000;
    const JOB: i32 = 54_001;
    const OTHER: i32 = 54_002;
    let (mut x, script, _c) = rig(SH, SH, shell(SH));
    script.post(&[exec(JOB, false), exec(OTHER, false)]);
    assert!(x.absorb(JOB, 0, || JOB));
    assert_eq!(script.follows().last(), Some(&Some(JOB)), "watched first");
    assert!(x.has_deferred(), "the other exec waits");
    script.post(&[exec(OTHER, false)]);
    assert!(!x.absorb(JOB, 0, || JOB), "no exec of the leader");
    set_image(claude(JOB));
    assert!(x.read_edge(JOB, true).drawer.is_some());
}

#[test]
fn an_exec_out_of_an_unknown_image_is_an_edge() {
    const SH: i32 = 55_000;
    const JOB: i32 = 55_001;
    let (watch, script) = ProcWatch::scripted(SH).expect("pipe");
    let mut x = ExecEdges::new(watch, scripted_read, JOB, no_ran(SH));
    assert_eq!(
        script.follows(),
        [Some(JOB)],
        "the adopted holder is followed"
    );
    let mut c = FgCutter::new(JOB);
    set_image(tool(JOB));
    c.begin();
    script.post(&[exec(JOB, false)]);
    assert!(x.heard(&mut c, || JOB, 0));
    assert_eq!(c.take_edge().map(|e| e.kind), Some(EdgeKind::Exec));
    c.begin();
    script.post(&[exec(JOB, false)]);
    assert!(!x.heard(&mut c, || JOB, 0));
}

#[test]
fn a_drain_reads_the_foreground_once_and_only_for_an_exec() {
    const SH: i32 = 56_000;
    const JOB: i32 = 56_001;
    let (mut x, script, mut c) = rig(SH, SH, shell(SH));
    set_image(tool(JOB));
    let mut reads = 0;
    c.begin();
    script.post(&[ProcEvent::Fork { child: JOB }, ProcEvent::Exit { pid: JOB }]);
    assert!(!x.heard(
        &mut c,
        || {
            reads += 1;
            SH
        },
        0
    ));
    assert_eq!(reads, 0, "no exec: no foreground read");
    script.post(&[exec(JOB, false), exec(JOB, false)]);
    assert!(!x.heard(
        &mut c,
        || {
            reads += 1;
            JOB
        },
        0
    ));
    assert_eq!(reads, 1, "two execs: one read");
}

#[test]
fn a_kernel_exec_reads_its_image_afresh() {
    const SH: i32 = 57_000;
    let (mut x, script, mut c) = rig(SH, SH, shell(SH));
    set_image(launcher(SH));
    c.begin();
    script.post(&[exec(SH, false)]);
    assert!(x.heard(&mut c, || SH, 0), "the shell became a launcher");
    assert_eq!(reads_of(SH), [None], "read with nothing to keep");
    c.begin();
    script.post(&[exec(SH, true)]);
    assert!(!x.heard(&mut c, || SH, 0), "the same image, late");
    assert_eq!(reads_of(SH), [None, None]);
}

#[test]
fn a_folded_exec_hands_its_image_to_the_sampled_edge() {
    const SH: i32 = 58_000;
    const TOOL: i32 = 58_001;
    const AGAIN: i32 = 58_002;
    const ABSORBED: i32 = 58_003;
    let (mut x, script, mut c) = rig(SH, SH, shell(SH));
    set_image(tool(TOOL));
    c.begin();
    script.post(&[ProcEvent::Fork { child: TOOL }, exec(TOOL, false)]);
    assert!(!x.heard(&mut c, || TOOL, 0), "a tool folds");
    let edge = c.finish(|| TOOL, 0).expect("the sampled edge");
    assert_eq!(edge.to, TOOL);
    assert!(!x.absorb(TOOL, 0, || TOOL));
    x.read_edge(TOOL, false);
    assert_eq!(
        reads_of(TOOL),
        [None, Some(tool(TOOL))],
        "the event's read, then the edge's given it to keep"
    );

    let (mut x, script, mut c) = rig(SH, SH, shell(SH));
    set_image(tool(AGAIN));
    c.begin();
    script.post(&[exec(AGAIN, false)]);
    assert!(!x.heard(&mut c, || AGAIN, 0));
    script.post(&[exec(AGAIN, false)]);
    assert!(
        !x.heard(&mut c, || SH, 0),
        "in the background: no candidate"
    );
    x.read_edge(AGAIN, false);
    assert_eq!(reads_of(AGAIN), [None, None]);

    let (mut x, script, mut c) = rig(SH, SH, shell(SH));
    set_image(tool(ABSORBED));
    c.begin();
    script.post(&[exec(ABSORBED, false)]);
    assert!(!x.heard(&mut c, || ABSORBED, 0));
    script.post(&[exec(ABSORBED, false)]);
    assert!(x.absorb(ABSORBED, 0, || ABSORBED));
    x.read_edge(ABSORBED, true);
    assert_eq!(reads_of(ABSORBED), [None, None]);
}

#[test]
fn a_drain_with_a_process_event_tells_the_kernel_edge() {
    static EDGE: crate::vitals_host::ProcEdge = crate::vitals_host::ProcEdge::new();
    const SH: i32 = 59_000;
    const JOB: i32 = 59_001;
    let (mut x, script, mut c) = rig(SH, SH, shell(SH));
    x.proc_edge = &EDGE;
    EDGE.arm(true);
    c.begin();
    assert!(!x.heard(&mut c, || SH, 0), "an empty drain");
    assert!(!EDGE.take(), "nothing heard: nothing told");

    set_image(tool(JOB));
    script.post(&[ProcEvent::Fork { child: JOB }]);
    c.begin();
    assert!(!x.heard(&mut c, || SH, 0), "a fork alone cuts nothing");
    assert!(EDGE.take(), "a fork tells it");

    EDGE.arm(true);
    script.post(&[ProcEvent::Exit { pid: JOB }]);
    assert!(!x.absorb(SH, 0, || SH), "an exit is no exec of the leader");
    assert!(EDGE.take(), "an exit heard while absorbing tells it too");
}

/// A scripted process's names: `p<pid>` for its file, `x<pid>` for its exec.
fn scripted_names(pid: i32, _steer: Steering<'_>) -> ExecNames {
    ExecNames {
        image: Some(format!("p{pid}").into()),
        exec: Some(format!("x{pid}").into()),
        ..ExecNames::default()
    }
}

/// Each `exec` a drain hears is noted once in the ran log, as it is drained: never the shell's,
/// never one whose group is not the terminal's foreground when heard (a background job's or an
/// async prompt hook's, which is no command's), and a replayed one not again. NEGATIVE CONTROL
/// (scratch): noting every exec whatever its group fails the list (the two background ones join
/// it).
#[test]
fn each_drained_exec_is_noted_once_in_the_ran_log_and_never_the_shells() {
    const SH: i32 = 60_000;
    const JOB: i32 = 60_001;
    const LATE: i32 = 60_002;
    const OTHER: i32 = 60_003;
    const BG: i32 = 60_004;
    const BG2: i32 = 60_005;
    let in_group = |pid: i32, pgid: i32, late: bool| ProcEvent::Exec { pid, pgid, late };
    let ran = std::sync::Arc::new(Ran::default());
    let (watch, script) = ProcWatch::scripted(SH).expect("pipe");
    let feed = RanFeed::reading(std::sync::Arc::clone(&ran), SH, scripted_names);
    let mut x = ExecEdges::new(watch, scripted_read, SH, feed);
    let mut c = FgCutter::new(SH);
    set_image(claude(JOB));
    c.begin();
    script.post(&[
        ProcEvent::Fork { child: JOB },
        exec(JOB, false),
        exec(SH, false),
        in_group(BG, BG, false),
        in_group(LATE, JOB, true),
    ]);
    assert!(x.heard(&mut c, || JOB, 0));
    assert!(
        x.has_deferred(),
        "the late exec waits behind the batch's edge"
    );
    let _ = c.take_edge();
    c.begin();
    let _ = x.replay(&mut c, || JOB);
    script.post(&[in_group(OTHER, JOB, false), in_group(BG2, BG2, false)]);
    let _ = x.absorb(JOB, 0, || JOB);
    let heard: Vec<(i32, i32, Option<String>)> = ran
        .within(Some(Window { from: 0, to: None }))
        .into_iter()
        .map(|e| (e.pid, e.pgid, e.names.exec.as_deref().map(String::from)))
        .collect();
    assert_eq!(
        heard,
        [
            (JOB, JOB, Some(format!("x{JOB}"))),
            (LATE, JOB, Some(format!("x{LATE}"))),
            (OTHER, JOB, Some(format!("x{OTHER}"))),
        ],
        "each foreground exec once, as drained (the replayed one not again), the shell's and the \
         background's never"
    );
}

/// A gather with no process watch (Linux, a watch that could not open) notes each holder image
/// it reads at a sampled foreground edge, by the image's file name, and never the shell's: the
/// session's one record of what ran there ([`crate::spawn::sampled_edge_image`]). NEGATIVE
/// CONTROL (scratch): the gather reading the image without noting it fails the first assertion.
#[test]
fn a_gather_with_no_watch_notes_each_sampled_holder_image_and_never_the_shells() {
    const SH: i32 = 61_000;
    const JOB: i32 = 61_001;
    let ran = std::sync::Arc::new(Ran::default());
    let feed = RanFeed::reading(std::sync::Arc::clone(&ran), SH, scripted_names);
    set_image(tool(JOB));
    set_image(shell(SH));
    let image = crate::spawn::sampled_edge_image(scripted_read, &feed, JOB, 0).expect("an edge");
    assert_eq!(image.pgid, JOB);
    let heard = || -> Vec<(i32, Option<String>)> {
        ran.within(Some(Window { from: 0, to: None }))
            .into_iter()
            .map(|e| (e.pgid, e.names.shown().map(str::to_owned)))
            .collect()
    };
    let file = tool(JOB)
        .exe
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    assert_eq!(
        heard(),
        [(JOB, file.clone())],
        "the in-front image, by its file"
    );
    let _ = crate::spawn::sampled_edge_image(scripted_read, &feed, SH, 0);
    assert!(
        crate::spawn::sampled_edge_image(scripted_read, &feed, 0, 0).is_none(),
        "no edge, no read"
    );
    assert_eq!(
        heard(),
        [(JOB, file)],
        "the shell's image is never a command's"
    );
}

/// H2 PLACEMENT: the PTY's unread bytes are read once a drain has heard an exec, before the
/// foreground is. An exec whose group still holds the foreground then is placed where the
/// shell's output stood (the bytes handed on, gathered and still queued): as a job's when its
/// group is not the shell's, else as the shell's own; one heard after a sampled edge whose group
/// no longer holds the foreground, or with no queue to read, is placed by its stamp alone.
/// NEGATIVE CONTROL (scratch): keeping the queue whatever group holds the foreground fails the
/// sampled edge's case with a cut at 19.
#[test]
fn an_exec_is_placed_by_the_queue_read_while_its_group_holds_the_foreground() {
    use crate::command_ran::{During, Places};
    const SH: i32 = 62_000;
    const JOB: i32 = 62_001;
    const KID: i32 = 62_002;
    const LOST: i32 = 62_003;
    let every = Some(Window { from: 0, to: None });
    let placed = |ran: &Ran, pid: i32| {
        ran.within(every)
            .into_iter()
            .find(|e| e.pid == pid)
            .map(|e| e.during)
    };
    // The gather hands on `len` bytes, which the parse stage parses with block `block` running
    // throughout: the cut each held exec waits at, then where each was placed.
    let parse = |ran: &Ran, len: usize, block: u64| {
        let end = ran.handed_on(len);
        let mut places = Places::default();
        ran.places_in(end, len, &mut places);
        let cut = places.next_cut(0);
        if let Some(cut) = cut {
            places.parsed(cut, Some(block));
        }
        places.parsed(len, Some(block));
        ran.parsed_at(end, len, &places, 50);
        cut
    };
    let ran = std::sync::Arc::new(Ran::default());
    let (watch, script) = ProcWatch::scripted(SH).expect("pipe");
    let feed = RanFeed::reading(std::sync::Arc::clone(&ran), SH, scripted_names);
    let mut x = ExecEdges::new(watch, scripted_read, SH, feed).reading_queue(|_| Some(10), -1);
    let mut c = FgCutter::new(SH);
    set_image(tool(JOB));
    c.begin();
    script.post(&[exec(JOB, false)]);
    let _ = x.heard(&mut c, || JOB, 3);
    assert_eq!(placed(&ran, JOB), None, "held until the parse reaches it");
    assert_eq!(
        parse(&ran, 20, 4),
        Some(13),
        "0 handed + 3 gathered + 10 queued"
    );
    assert_eq!(placed(&ran, JOB), Some(During::Job(Some(4))));
    let _ = c.take_edge();
    c.begin();
    script.post(&[ProcEvent::Exec {
        pid: KID,
        pgid: SH,
        late: false,
    }]);
    let _ = x.heard(&mut c, || SH, 0);
    assert_eq!(
        parse(&ran, 20, 5),
        Some(10),
        "20 handed + 10 queued, inside the batch"
    );
    assert_eq!(
        placed(&ran, KID),
        Some(During::Shell(Some(5))),
        "the shell's own group: where its output stood at most"
    );
    script.post(&[exec(LOST, false)]);
    let _ = x.absorb(LOST, 9, || SH);
    assert_eq!(parse(&ran, 20, 6), None, "no queue kept: no cut");
    assert_eq!(
        placed(&ran, LOST),
        Some(During::Window),
        "its group lost the foreground: by its stamp"
    );
    let unread = std::sync::Arc::new(Ran::default());
    let (watch, script) = ProcWatch::scripted(SH).expect("pipe");
    let feed = RanFeed::reading(std::sync::Arc::clone(&unread), SH, scripted_names);
    let mut x = ExecEdges::new(watch, scripted_read, SH, feed);
    let mut c = FgCutter::new(SH);
    c.begin();
    script.post(&[exec(JOB, false)]);
    let _ = x.heard(&mut c, || JOB, 3);
    let _ = parse(&unread, 20, 4);
    assert_eq!(
        placed(&unread, JOB),
        Some(During::Window),
        "no queue to read: by its stamp"
    );
}
