// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! THE CHIP AS DRAWN (`docs/DESIGN-harness-autopilot-2026-10-15.md` §12.5.3
//! and §12.5.6, slice 5b): `chrome`'s `autopilot` line says the chip's face
//! now and its cells only as the last frame drew them — never a stale face
//! for a pane that frame did not draw, never a chip a later band covers,
//! never a face for a program with no footer. Driven on the real engine
//! through the capture route's splices, and read against the frame's own
//! cells and the accessibility tree's own nodes.

use std::time::Instant;

use aterm_agent::supervise::Autopilot;

use super::super::gesture_tests::{
    BYPASS, Hosted, PGID, app, cols, draw_claude, feed, frame, host, publish_facts,
};
use super::{Drawn, WindowLights};
use crate::autopilot::AutopilotView;
use crate::{App, WindowId};

/// Session `session` runs `program` (its published foreground name), as the
/// status sweep publishes it.
fn set_program(app: &App, session: u64, program: &str) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    let mut timeline = ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(PGID);
    timeline.set_program(PGID, Some(program.into()));
}

/// Session `session` runs Claude Code with its facts published and its
/// composer drawn: what the footer, and so the chip, is painted for.
fn claude(app: &App, session: u64) {
    publish_facts(app, session);
    set_program(app, session, "claude");
    draw_claude(app, session, BYPASS);
}

/// Session `session`'s own Autopilot word, written as `meta set autopilot`
/// stores it — with no frame painted after it.
fn set_word(app: &App, session: u64, word: Autopilot) {
    app.pool
        .get(session)
        .expect("session")
        .ctx
        .meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set("autopilot", Some(word.word().into()));
}

/// A hosted window: one Claude Code pane, drawn, one frame painted.
fn desk() -> (App, WindowId, u64, std::os::unix::net::UnixStream, Hosted) {
    let (mut app, wid, session, reader) = app();
    let hosted = host(&mut app);
    claude(&app, session);
    frame(&mut app, wid);
    (app, wid, session, reader, hosted)
}

/// Session `session`'s `autopilot` lines, in the order `chrome` prints them.
fn lines_of(app: &App, session: u64) -> Vec<String> {
    let sid = app.pool.get(session).expect("session").ctx.self_id.clone();
    let head = format!("autopilot sid={} ", sid.as_str());
    app.drawn_chip_lines()
        .into_iter()
        .filter(|l| l.starts_with(&head))
        .collect()
}

/// Session `session`'s `autopilot` line, exactly one.
fn line_of(app: &App, session: u64) -> String {
    let lines = lines_of(app, session);
    assert_eq!(lines.len(), 1, "one line for the pane: {lines:?}");
    lines[0].clone()
}

/// The line's bare `key=` token.
fn token<'a>(line: &'a str, key: &str) -> &'a str {
    let at = line
        .find(&format!(" {key}="))
        .unwrap_or_else(|| panic!("{key}= in {line:?}"));
    let rest = &line[at + key.len() + 2..];
    rest.split(' ').next().unwrap_or_default()
}

/// The line's quoted `key="…"`, `None` for `key=-`.
fn quoted(line: &str, key: &str) -> Option<String> {
    let at = line
        .find(&format!(" {key}="))
        .unwrap_or_else(|| panic!("{key}= in {line:?}"))
        + key.len()
        + 2;
    let rest = &line[at..];
    let quoted = rest.strip_prefix('"')?;
    let end = quoted.find('"').expect("a quoted value closes");
    Some(quoted[..end].to_string())
}

/// The line's `drawn=`: the quoted cells, or `None` for `drawn=-`.
fn drawn(line: &str) -> Option<String> {
    quoted(line, "drawn")
}

/// The Autopilot chip of `session` as the last frame put it on the glass of
/// window `wid`: the frame's own cells at the chip's hits, in column order,
/// a wide glyph's right half left out. `None` where no hit of it stands.
fn on_glass(app: &App, wid: WindowId, session: u64) -> Option<String> {
    let ws = &app.windows[&wid];
    let mut hits: Vec<(usize, usize)> = ws
        .claude_lights
        .hits
        .iter()
        .filter(|h| h.session == session && h.chip == Some(super::Chip::Autopilot))
        .map(|h| (h.frame_row, h.col))
        .collect();
    hits.sort_unstable();
    (!hits.is_empty()).then(|| {
        hits.iter()
            .map(|&(row, col)| ws.input_scratch.cells[row][col])
            .filter(|c| !c.wide)
            .map(|c| c.ch)
            .collect()
    })
}

/// What the accessibility tree's node for `session`'s Autopilot chip on
/// window `wid` is called (`App::claude_chips_a11y`), `None` where the tree
/// has no such node.
#[cfg(a11y_tree)]
fn node_label(app: &App, wid: WindowId, session: u64) -> Option<String> {
    app.claude_chips_a11y(wid)
        .into_iter()
        .find(|c| c.session == session && c.chip == crate::a11y_tree::HarnessChip::Autopilot)
        .map(|c| c.label)
}

/// THE FACE MOVES, THE GLASS DOES NOT UNTIL A FRAME: with no frame since
/// the session's word moved, `face=` says the new face, `drawn=` the face
/// the frame drew, cell for cell what the frame's glass holds, and
/// `spoken=` what the chip's accessibility node says of it; after a frame
/// all three say the new one. NEGATIVE CONTROLS: `drawn=` read from
/// [`super::ChipFace::of`] (the face now) says `⤓ land` before any frame
/// drew it; `spoken=` spoken from the face now says land while the node
/// still says cruise; each fails the second block.
#[test]
fn chrome_reads_the_chip_the_frame_drew() {
    let (mut app, wid, session, _reader, _host) = desk();
    let line = line_of(&app, session);
    assert_eq!(token(&line, "window"), "0", "{line}");
    assert_eq!(token(&line, "program"), "claude", "{line}");
    assert_eq!(token(&line, "face"), "cruise", "{line}");
    assert_eq!(drawn(&line).as_deref(), Some("\u{221E}"), "{line}");
    assert_eq!(
        drawn(&line),
        on_glass(&app, wid, session),
        "drawn= is the frame's cells: {line}"
    );
    assert!(
        quoted(&line, "spoken").is_some(),
        "a drawn chip speaks: {line}"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        quoted(&line, "spoken"),
        node_label(&app, wid, session),
        "spoken= is the node's label: {line}"
    );
    let before = quoted(&line, "spoken");

    set_word(&app, session, Autopilot::Land);
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "land", "the face now: {line}");
    assert_eq!(
        drawn(&line).as_deref(),
        Some("\u{221E}"),
        "no frame since: the drawn face: {line}"
    );
    assert_eq!(drawn(&line), on_glass(&app, wid, session), "{line}");
    assert_eq!(
        quoted(&line, "spoken"),
        before,
        "no frame since: the drawn chip's words: {line}"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        quoted(&line, "spoken"),
        node_label(&app, wid, session),
        "{line}"
    );

    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "land", "{line}");
    assert_eq!(
        drawn(&line).as_deref(),
        Some("\u{2913} land"),
        "the frame drew the new face: {line}"
    );
    assert_eq!(drawn(&line), on_glass(&app, wid, session), "{line}");
    assert_ne!(
        quoted(&line, "spoken"),
        before,
        "the new face's words: {line}"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        quoted(&line, "spoken"),
        node_label(&app, wid, session),
        "{line}"
    );
}

/// A PANE BEHIND NAMES NO DRAWN CHIP: two tabs, each a Claude Code; with the
/// second in front its line's `drawn=` is the frame's cells, while the first
/// — its chip's face moved behind — says `drawn=- why=not-drawn spoken=-`
/// and the new `face=`, before a frame (the frame that drew it is the last
/// one) and after one. Brought back in front with no frame since, it still
/// says not-drawn: the last frame drew the other tab. After a frame it says
/// the new face. NEGATIVE CONTROLS: the pane's leaf looked up in its own
/// tab's plan instead of the front tab's says the old `∞` while the tab is
/// behind; a block read without `used` says `why=no-room` once the tab is
/// back in front with no frame; one that prints the kept block whatever the
/// last frame did (no `used`, no hit) says the old `∞` there.
#[test]
fn a_background_pane_names_no_drawn_chip() {
    let (mut app, wid, a, _reader, _host) = desk();
    assert_eq!(drawn(&line_of(&app, a)).as_deref(), Some("\u{221E}"));

    // A second tab, in front, running a Claude Code of its own.
    let b = app.next_session_id;
    app.push_stub_tab(wid, crate::stub_session(b));
    assert_eq!(app.focused_session_id(wid), Some(b), "the new tab in front");
    claude(&app, b);
    let line = line_of(&app, a);
    assert_eq!(
        (drawn(&line), token(&line, "why")),
        (None, "not-drawn"),
        "behind, before a frame: {line}"
    );

    set_word(&app, a, Autopilot::Land);
    frame(&mut app, wid);
    let behind = line_of(&app, a);
    assert_eq!(token(&behind, "face"), "land", "the face now: {behind}");
    assert_eq!(
        (drawn(&behind), token(&behind, "why")),
        (None, "not-drawn"),
        "never the stale face: {behind}"
    );
    assert_eq!(token(&behind, "spoken"), "-", "no chip, no words: {behind}");
    let front = line_of(&app, b);
    assert_eq!(drawn(&front).as_deref(), Some("\u{221E}"), "{front}");
    assert_eq!(
        drawn(&front),
        on_glass(&app, wid, b),
        "the front tab's drawn= is the frame's cells: {front}"
    );

    // Back in front, no frame since: the last frame drew the other tab.
    app.switch_tab_in(wid, 0);
    assert_eq!(
        app.focused_session_id(wid),
        Some(a),
        "the first tab in front"
    );
    let line = line_of(&app, a);
    assert_eq!(
        (drawn(&line), token(&line, "why")),
        (None, "not-drawn"),
        "no frame has drawn it since: {line}"
    );
    let line = line_of(&app, b);
    assert_eq!(token(&line, "why"), "not-drawn", "now behind: {line}");

    frame(&mut app, wid);
    let line = line_of(&app, a);
    assert_eq!(drawn(&line).as_deref(), Some("\u{2913} land"), "{line}");
    assert_eq!(drawn(&line), on_glass(&app, wid, a), "{line}");
}

/// A CHIP A LATER BAND COVERS IS NOT DRAWN: a band the frame splices after
/// the footer (the link caption, the paste question) claims the rule's row
/// in the window's chrome register; the pointer's, the keyboard's and the
/// accessibility tree's readers of the hits drop the chip there, and
/// `chrome` says `drawn=- why=not-drawn spoken=-` too. NEGATIVE CONTROL: the
/// register ignored prints `drawn="∞"` for a chip nobody can see.
#[test]
fn a_chip_a_band_covers_names_no_cells() {
    let (mut app, wid, session, _reader, _host) = desk();
    assert_eq!(drawn(&line_of(&app, session)).as_deref(), Some("\u{221E}"));
    let row = app.windows[&wid]
        .claude_lights
        .hits
        .iter()
        .find(|h| h.session == session && h.chip == Some(super::Chip::Autopilot))
        .expect("the chip's hit")
        .term_row;
    // What `claim_chrome_rows` records for a band over that row.
    app.windows
        .get_mut(&wid)
        .expect("window")
        .chrome_rows
        .push(row..row + 1);
    assert!(
        app.chrome_owns_terminal_row(wid, row),
        "the band owns the rule's row"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        node_label(&app, wid, session),
        None,
        "the tree drops it too"
    );
    let line = line_of(&app, session);
    assert_eq!(
        (drawn(&line), token(&line, "why"), token(&line, "spoken")),
        (None, "not-drawn", "-"),
        "a covered chip is not drawn: {line}"
    );
    assert_eq!(token(&line, "face"), "cruise", "the face stands: {line}");
}

/// A RULE WITH NO RUN TO WRITE INTO HAS NO ROOM: Claude Code's bottom rule
/// on the glass with a mark on every glyph, so no cell of it is plain rule
/// for the footer to take — the frame finds the rule (the keyboard's
/// register, `footers`) and fits no block — says `drawn=- why=no-room`
/// under a live host, whatever no frame has noted of the session's view.
/// NEGATIVE CONTROLS: the register's clause dropped says `not-drawn` (the
/// rule is on the glass); the host judged from a view no frame noted says
/// `no-host`.
#[test]
fn a_rule_with_no_run_names_no_room() {
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    publish_facts(&app, session);
    set_program(&app, session, "claude");
    let n = cols(&app, session);
    let top = "\u{2500}".repeat(n);
    let marked = "\u{2500}\u{0301}".repeat(n);
    let screen = format!(
        "\x1b[?2004h\x1b[2J\x1b[19;1H\u{25CF} done\x1b[21;1H{top}\x1b[22;1H\u{276F} \
         \x1b[23;1H{marked}\x1b[24;1H{BYPASS}\x1b[22;3H"
    );
    feed(&app, session, screen.as_bytes());
    frame(&mut app, wid);
    let lights = &app.windows[&wid].claude_lights;
    assert!(
        lights
            .footers
            .iter()
            .any(|(s, room)| *s == session && *room == 0),
        "the rule is on the glass with no room: {:?}",
        lights.footers
    );
    assert!(
        !lights.fitted.iter().any(|f| f.session == session && f.used),
        "no block is fitted for it"
    );
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "cruise", "a live host: {line}");
    assert_eq!(
        (drawn(&line), token(&line, "why")),
        (None, "no-room"),
        "{line}"
    );
}

/// A SESSION CO-VIEWED IN TWO WINDOWS PRINTS A LINE IN EACH, TOLD APART BY
/// `window=`: each window's line says the cells that window's last frame
/// drew. NEGATIVE CONTROL: a line with no `window=` leaves the two lines
/// one sid and nothing between them, and fails.
#[test]
fn a_co_viewed_session_names_each_window() {
    let (mut app, wid, session, _reader, _host) = desk();
    let other = app
        .open_active_session_in_new_window_logical()
        .expect("the session co-viewed in a second window");
    frame(&mut app, other);
    frame(&mut app, wid);
    let lines = lines_of(&app, session);
    assert_eq!(lines.len(), 2, "a line in each window: {lines:?}");
    let windows: Vec<&str> = lines.iter().map(|l| token(l, "window")).collect();
    let mut want = vec![wid.0.to_string(), other.0.to_string()];
    want.sort();
    let mut got: Vec<String> = windows.iter().map(|w| (*w).to_string()).collect();
    got.sort();
    assert_eq!(got, want, "{lines:?}");
    for line in &lines {
        let window = WindowId(token(line, "window").parse().expect("a window id"));
        assert_eq!(
            drawn(line),
            on_glass(&app, window, session),
            "each window's own frame: {line}"
        );
    }
}

/// GEMINI CLI HAS NO FOOTER, SO NO CHIP: its line says `face=- drawn=-
/// why=no-footer spoken=- description=-`, its mode notwithstanding. NEGATIVE CONTROL:
/// `drawn_for` ignored prints its face (`face=cruise`); and the same pane
/// running Claude Code prints one.
#[test]
fn the_chip_line_names_no_chip_for_gemini() {
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    set_program(&app, session, "gemini");
    frame(&mut app, wid);
    let line = line_of(&app, session);
    let sid = app.pool.get(session).expect("session").ctx.self_id.clone();
    assert_eq!(
        line,
        format!(
            "autopilot sid={} window=0 program=gemini face=- drawn=- why=no-footer spoken=- \
             description=-",
            sid.as_str()
        )
    );
    // Control: the same pane running Claude Code wears a face.
    claude(&app, session);
    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "cruise", "{line}");
}

/// Session `session`'s harness need, as the loop's request lands it: the
/// needs owner's entry (`None`: its `unset`) and its `MetaChanged` wake,
/// which publishes the need.
fn need(app: &mut App, session: u64, entry: Option<&str>) {
    let ctx = app.pool.get(session).expect("session").ctx.clone();
    ctx.meta
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .set_attention_kind(
            crate::harness_host::NEEDS_ATTENTION_OWNER,
            entry.map(str::to_string),
            entry.map(|_| aterm_types::needs::AttentionKind::Ask),
        )
        .expect("a slot");
    app.on_meta_changed(session);
}

/// What the accessibility tree's node for `session`'s Autopilot chip on
/// window `wid` describes it as, `None` where it has no description.
#[cfg(a11y_tree)]
fn node_description(app: &App, wid: WindowId, session: u64) -> Option<String> {
    app.claude_chips_a11y(wid)
        .into_iter()
        .find(|c| c.session == session && c.chip == crate::a11y_tree::HarnessChip::Autopilot)
        .and_then(|c| c.description)
}

/// CHROME SAYS THE CHIP'S DESCRIPTION AS ITS NODE DOES (the W4 title
/// slice, §12.2.1 and §12.3.1): a Cruise chip with nothing to say beside
/// its face prints `description=-`; with a harness's need published,
/// `description="Needs you: <the need>. Carrying on."`; under `⤓ land` with
/// a read hold, `spoken="Autopilot: land."` still (the painted word, WCAG
/// 2.5.3) and a description that leads `Not landed yet.` and names the
/// need in the hold, once; each the accessibility node's own. A worker's
/// `▸` is said as a comma against the word before it. NEGATIVE CONTROL (as
/// run): `drawn_chip_line` printing `description=-` whatever the node says
/// fails the need's line.
#[test]
fn chrome_says_the_chips_description_as_its_node_does() {
    use crate::harness_glass::hold_tests::held;
    let (mut app, wid, session, _reader, _host) = desk();
    let line = line_of(&app, session);
    assert_eq!(token(&line, "description"), "-", "{line}");
    // A need with the worker's own `▸`: said a comma against its word.
    need(
        &mut app,
        session,
        Some(
            "claude needs you: grant Screen Recording in System Settings \u{25b8} Privacy & \
             Security \u{2014} carrying on",
        ),
    );
    let line = line_of(&app, session);
    assert_eq!(
        quoted(&line, "description").as_deref(),
        Some(
            "Needs you: grant Screen Recording in System Settings, Privacy and Security. Carrying on."
        ),
        "{line}"
    );
    need(
        &mut app,
        session,
        Some("claude needs you: approve the 0.48.0 release notes \u{2014} carrying on"),
    );
    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(
        quoted(&line, "spoken").as_deref(),
        Some("Autopilot: cruise.")
    );
    assert_eq!(
        quoted(&line, "description").as_deref(),
        Some("Needs you: approve the 0.48.0 release notes. Carrying on."),
        "{line}"
    );
    #[cfg(a11y_tree)]
    assert_eq!(
        quoted(&line, "description"),
        node_description(&app, wid, session),
        "{line}"
    );
    set_word(&app, session, Autopilot::Land);
    assert!(crate::harness_glass::on_land_held(
        &mut app,
        session,
        Some(held())
    ));
    frame(&mut app, wid);
    let line = line_of(&app, session);
    assert_eq!(token(&line, "face"), "land", "{line}");
    assert_eq!(quoted(&line, "spoken").as_deref(), Some("Autopilot: land."));
    let description = quoted(&line, "description").expect("described");
    assert!(
        description.starts_with(
            "Not landed yet. Needs you: approve the 0.48.0 release notes. 2 unpushed commits. "
        ),
        "{line}"
    );
    assert_eq!(description.matches("Needs you").count(), 1, "{line}");
    #[cfg(a11y_tree)]
    assert_eq!(
        Some(description),
        node_description(&app, wid, session),
        "{line}"
    );
    // The need taken down: the hold names one earlier in its reply.
    need(&mut app, session, None);
    let line = line_of(&app, session);
    assert!(
        quoted(&line, "description")
            .is_some_and(|d| d.starts_with("Not landed yet. A need named earlier in its reply.")),
        "{line}"
    );
}

/// THE HOVERED TITLE ON THE GLASS SAYS THE NEED AND THE HOLD (§12.2.3
/// item 5, §12.3.3 item 4; the W4 title slice's review): pointed at on a
/// rule with room for the whole title, a Cruise chip's title as the frame
/// drew it carries the session's published need as `need_title` makes it,
/// and under `⤓ land` with a read hold, the hold's title form right after
/// the mode phrase — what the block read off the session's own glass and
/// timeline, where the face tests hand `autopilot_title` its words
/// themselves. NEGATIVE CONTROL (as run): the block's `read_words`
/// answering `(None, None)` draws the title without the need and fails the
/// first assert.
#[test]
fn the_hovered_title_on_the_glass_says_the_need_and_the_hold() {
    use super::super::gesture_tests::{footer_row, row_text};
    use crate::harness_glass::{HoldForm, SessionGlass, hold_tests::held};
    let (mut app, wid, session, _reader, _host) = desk();
    assert!(app.apply_term_resize(wid, 24, 400));
    draw_claude(&app, session, BYPASS);
    need(
        &mut app,
        session,
        Some("claude needs you: approve the 0.48.0 release notes \u{2014} carrying on"),
    );
    let ask = app
        .pool
        .get(session)
        .expect("session")
        .ctx
        .timeline
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .need()
        .harness_ask()
        .cloned()
        .expect("CONTROL: the need is a harness's ask");
    app.hover_claude_chip(wid, Some((session, super::Chip::Autopilot)));
    frame(&mut app, wid);
    let rule = row_text(&app, wid, footer_row(&app, wid));
    let said = crate::harness_glass::need_title(&ask);
    assert!(
        rule.contains(&said),
        "the need {said:?} on the glass: {rule:?}"
    );
    set_word(&app, session, Autopilot::Land);
    assert!(crate::harness_glass::on_land_held(
        &mut app,
        session,
        Some(held())
    ));
    frame(&mut app, wid);
    let rule = row_text(&app, wid, footer_row(&app, wid));
    let hold = app
        .harness_glass
        .get(&session)
        .and_then(SessionGlass::hold)
        .expect("CONTROL: the hold on the session's glass");
    let today = Some(crate::harness_glass::wall_today().1);
    let held_words = crate::harness_glass::hold_words(&hold, Some(&ask), HoldForm::Title, today)
        .expect("a read hold's words");
    assert!(
        rule.contains(&format!("autopilot: land \u{2014} {held_words}")),
        "the hold {held_words:?} on the glass: {rule:?}"
    );
}

/// A PANE NO SUPERVISOR ADMITS PRINTS NO LINE: a shell's pane, beside a
/// Claude Code's, which does.
#[test]
fn a_shell_pane_prints_no_chip_line() {
    let (mut app, wid, session, _reader) = app();
    let _host = host(&mut app);
    set_program(&app, session, "zsh");
    frame(&mut app, wid);
    assert!(app.drawn_chip_lines().is_empty(), "no agent, no line");
    claude(&app, session);
    frame(&mut app, wid);
    assert_eq!(
        app.drawn_chip_lines().len(),
        1,
        "control: Claude Code's pane has one"
    );
}

/// CELLS ONLY WHERE THE CHIP REACHED THE GLASS: a block the frame fitted
/// and lent, and that came back, but whose chips the rule's write never took
/// (no hit of them), says `no-room`; once the painter notes its hits, the
/// cells. A pane whose session has no host says `no-host`. NEGATIVE
/// CONTROL: the hit rule removed prints `∞` for the block the rule did not
/// take, and fails the first half.
#[test]
fn a_chip_the_rule_did_not_take_names_no_cells() {
    let blank = aterm_core::terminal::RenderCell {
        ch: ' ',
        ..aterm_core::terminal::RenderCell::default()
    };
    let now = Instant::now();
    let shown = |_: usize| false;
    let mut w = WindowLights::default();
    w.note_autopilot(7, None, AutopilotView::Cruise, now);
    w.clear_hits();
    let block = w
        .lend_block(7, 0, blank, [0, 128, 255], (None, None), 40, None)
        .expect("a chip fits in 40 cells");
    w.give_back(block);
    assert_eq!(
        w.drawn_chip(7, 0, shown),
        Drawn::NoRoom,
        "lent, never on the glass"
    );
    assert_eq!(
        w.drawn_chip(7, 1, shown),
        Drawn::NoRule,
        "another leaf: not drawn"
    );

    // The painter notes the chip's hits where the write took its row.
    let painted = w.fitted[0].block.as_ref().expect("the block is home");
    let cols = painted.cols.clone();
    for (span, chip) in cols {
        for at in span {
            w.hits.push(super::super::LightHit {
                frame_row: 30,
                term_row: 22,
                col: 60 + at,
                session: 7,
                chip: Some(chip),
            });
        }
    }
    assert_eq!(w.drawn_chip(7, 0, shown), Drawn::Cells("\u{221E}".into()));
    assert_eq!(
        w.drawn_chip(7, 0, |row| row == 22),
        Drawn::NoRule,
        "a band over row 22 hides it"
    );

    // No host: the rule is drawn and no chip is (`face=unhosted`).
    let mut w = WindowLights::default();
    w.note_autopilot(8, None, AutopilotView::Unhosted, now);
    w.clear_hits();
    assert!(
        w.lend_block(8, 0, blank, [0, 128, 255], (None, None), 40, None)
            .is_none(),
        "control: no host, no chip"
    );
    assert_eq!(w.drawn_chip(8, 0, shown), Drawn::NoHost);
}
