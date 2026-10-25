// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-frame Claude Code footer pass fixture ([`footer_pass_cost`]).

use super::*;

use std::time::Instant;

use aterm_agent::harness::footer::FactsOwner;
use aterm_agent::harness::session_usage::{ModelTokens, More, UsageFacts};

/// The pane: an idle Claude Code session well into a conversation.
pub(crate) const ROWS: u16 = 40;
pub(crate) const COLS: u16 = 120;

/// Bypass, the mode the owner expects: no light is drawn.
pub(crate) const BYPASS: &str = "  \u{23F5}\u{23F5} bypass permissions on (shift+tab to cycle)";
/// The last turn's done row, the status row over the composer.
pub(crate) const DONE: &str = "\u{273B} Baked for 4s \u{b7} done 2:41 PM";

/// What a screen shows under the transcript.
#[derive(Clone, Copy)]
pub(crate) struct Live<'a> {
    pub(crate) status: &'a str,
    pub(crate) draft: &'a [&'a str],
    pub(crate) under: &'a [&'a str],
}

pub(crate) const AT_REST: Live<'static> = Live {
    status: DONE,
    draft: &[""],
    under: &[BYPASS],
};

impl Live<'_> {
    /// Row index of the composer's bottom rule.
    pub(crate) fn bottom(&self) -> usize {
        usize::from(ROWS) - 1 - self.under.len()
    }
}

/// A Claude Code screen `ROWS` × `COLS` well into a conversation.
pub(crate) fn screen(live: &Live<'_>) -> Vec<String> {
    let (rows, cols) = (usize::from(ROWS), usize::from(COLS));
    let rule = "\u{2500}".repeat(cols);
    let mut s: Vec<String> = (0..rows - 4 - live.draft.len() - live.under.len())
        .map(|i| match i % 4 {
            0 => format!("\u{23FA} Message {i}: what Claude said about the change, as it says it."),
            3 => String::new(),
            _ => format!("  line {i} of it, continued at column two as Claude writes a message."),
        })
        .collect();
    s.extend([live.status.to_owned(), String::new(), rule.clone()]);
    for (k, row) in live.draft.iter().enumerate() {
        s.push(if k == 0 {
            format!("\u{276F} {row}")
        } else {
            format!("  {row}")
        });
    }
    s.push(rule);
    s.extend(live.under.iter().map(|row| (*row).to_owned()));
    assert_eq!(s.len(), rows);
    s
}

/// Paint `rows` into `session`'s terminal, the rules in Claude's gray, the caret on the composer.
pub(crate) fn feed(app: &App, session: u64, rows: &[String]) {
    bytes(app, session, paint(rows).as_bytes());
}

/// The bytes [`feed`] paints `rows` with.
pub(crate) fn paint(rows: &[String]) -> String {
    let mut out = String::from("\x1b[2J");
    for (i, row) in rows.iter().enumerate() {
        let sgr = if row.starts_with('\u{2500}') {
            "\x1b[38;2;90;90;90m"
        } else {
            ""
        };
        out.push_str(&format!("\x1b[{};1H{sgr}{row}\x1b[0m", i + 1));
    }
    let caret = rows
        .iter()
        .rposition(|r| r.starts_with('\u{276F}'))
        .unwrap_or(0);
    out.push_str(&format!("\x1b[{};3H", caret + 1));
    out
}

/// [`AT_REST`] with Claude's `left`% context reading over the composer.
#[cfg(test)]
pub(crate) fn context_screen(left: u8) -> Vec<String> {
    let mut rows = screen(&AT_REST);
    let top = rows
        .iter()
        .position(|r| r.starts_with('\u{2500}'))
        .expect("the composer's top rule");
    let text = format!("{left}% until auto-compact");
    rows[top - 1] = format!("{}{text}", " ".repeat(usize::from(COLS) - 2 - text.len()));
    rows
}

pub(crate) fn bytes(app: &App, session: u64, bytes: &[u8]) {
    let term = app.pool.get(session).expect("session").term.clone();
    crate::term_lock(&term).process(bytes);
}

/// The facts a long-running session shows.
pub(crate) fn facts() -> FooterFacts {
    FooterFacts {
        model: Some("Opus 5.5".into()),
        effort: Some("xhigh".into()),
        path: Some("~/aterm".into()),
        branch: Some("main".into()),
        version: Some("2.1.283".into()),
        usage: usage(48_000_000),
        owner: Some(FactsOwner {
            floor: Some(0),
            session_id: "0b5e7c1d-9a1f-4c2e-8d1b-2f3a4b5c6d7e".into(),
        }),
        ..FooterFacts::default()
    }
}

pub(crate) fn usage(input: u64) -> Option<UsageFacts> {
    UsageFacts::of(
        (
            vec![ModelTokens {
                label: "opus".into(),
                input,
                output: 310_000,
            }],
            More::default(),
        ),
        None,
        None,
    )
}

pub(crate) fn publish(app: &App, session: u64, facts: FooterFacts) {
    let entry = app.pool.get(session).expect("session");
    let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
    timeline.note_foreground_group(4242);
    assert!(timeline.set_claude_footer(4242, None, Some(facts)));
}

/// Transitions `session` to foreground card drawer `program` (`version`, `pgid`).
#[cfg(test)]
pub(crate) fn drawer(app: &App, session: u64, pgid: i32, program: &str, version: &str) -> u64 {
    use atpkg::image::{ImageBuild, ImageSource, ProgramTag};
    let build = atpkg::vendor_direct::Version::parse(version)
        .expect("a vendor build")
        .build_id();
    let image = crate::holder_image::HolderImage::of(
        pgid,
        Some(1),
        Some(std::path::PathBuf::from(format!(
            "/p/pkg/store/{program}/{build}/bin/{program}"
        ))),
        ImageBuild {
            program: ProgramTag::new(program),
            build,
            source: ImageSource::Store,
        },
        1,
    );
    let token = image.token.expect("a token");
    let entry = app.pool.get(session).expect("session");
    {
        let mut timeline = entry.ctx.timeline.lock().unwrap_or_else(|p| p.into_inner());
        timeline.note_foreground_group(pgid);
        timeline.offer_holder_image(
            std::sync::Arc::new(image),
            crate::holder_image::EdgeKind::Group,
        );
    }
    crate::term_lock(&entry.term).note_holder_cut(aterm_core::terminal::HolderCut {
        to: Some(token),
        kind: aterm_core::terminal::CutKind::Group,
        from_alive: false,
    });
    token
}

/// Builds a single-pane Claude Code window (`ROWS` by `COLS`) showing `live` and `facts`.
pub(crate) fn glass_with(live: &Live<'_>, facts: FooterFacts) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let session = app.focused_session_id(wid).expect("the front session");
    assert!(app.apply_term_resize(wid, ROWS, COLS));
    feed(&app, session, &screen(live));
    publish(&app, session, facts);
    for _ in 0..2 {
        refill(&mut app, wid);
        let _ = pass(&mut app, wid);
    }
    (app, wid, session)
}

pub(crate) fn probe_glass() -> (App, WindowId, u64) {
    glass_with(&AT_REST, facts())
}

/// Extract the frame and prepend the strip, as the capture route does.
pub(crate) fn refill(app: &mut App, wid: WindowId) {
    app.capture_words(wid, Instant::now());
    app.splice_tab_strip(wid);
}

/// The footer's pass: its repaint key and its splice ([`refill`]).
pub(crate) fn pass(app: &mut App, wid: WindowId) -> u64 {
    let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
    pass_on(app, wid, &plan, route)
}

pub(crate) fn pass_on(
    app: &mut App,
    wid: WindowId,
    plan: &crate::tab_model::VisibleLeafPlan,
    route: VisibleContentRoute,
) -> u64 {
    let fp = app.claude_footer_fp(wid, plan);
    app.splice_claude_footer(wid, plan, route);
    fp
}

/// A change to the pane's screen, the `i`th of its kind.
pub(crate) type Change = fn(&App, u64, usize);

/// A key typed into the composer: its echo at the caret.
pub(crate) fn keystroke(app: &App, session: u64, i: usize) {
    let caret = AT_REST.bottom();
    let ch = char::from(b'a' + (i % 26) as u8);
    let col = 3 + i % 40;
    bytes(
        app,
        session,
        format!("\x1b7\x1b[{caret};{col}H{ch}\x1b8").as_bytes(),
    );
}

/// A row Claude streams into the transcript, well above the status row.
pub(crate) fn streamed(app: &App, session: u64, i: usize) {
    let row = 5 + i % 20;
    bytes(
        app,
        session,
        format!(
            "\x1b7\x1b[{row};1H\u{23FA} streamed line {i}: the next words of the reply\x1b[K\x1b8"
        )
        .as_bytes(),
    );
}

/// A turn running: the status row's spinner ticks (and its seconds).
pub(crate) fn spinner(app: &App, session: u64, i: usize) {
    let status = AT_REST.bottom() - 3;
    let glyph = ['\u{2722}', '\u{2733}', '\u{2736}', '\u{273B}'][i % 4];
    bytes(
        app,
        session,
        format!(
            "\x1b7\x1b[{status};1H{glyph} Thinking\u{2026} ({}s \u{b7} esc to interrupt)\x1b[K\x1b8",
            i / 10
        )
        .as_bytes(),
    );
}

/// The window's harness host, for the harness's chips to be drawn.
pub(crate) struct Hosted(crate::harness_host::HostHandle);

impl Drop for Hosted {
    fn drop(&mut self) {
        self.0.shutdown_and_join();
    }
}

/// Give `app` a [`Hosted`] harness host, so the rule draws the harness's chips.
pub(crate) fn hosted(app: &mut App) -> Hosted {
    let host = crate::harness_host::HostHandle::for_supervised_test(
        Vec::new(),
        std::sync::Arc::new(|_| crate::harness_host::BodyEnd::Stopped),
    );
    app.harness = Some(host.clone());
    Hosted(host)
}

/// One kind of frame's footer pass, timed: µs per pass at p50, p90 and p99.
#[cfg(feature = "bench-support")]
#[derive(Clone, Copy, Debug)]
pub(crate) struct PassCost {
    pub(crate) what: &'static str,
    pub(crate) p50_us: f64,
    pub(crate) p90_us: f64,
    pub(crate) p99_us: f64,
}

/// What the pointer is on in a probed frame.
#[cfg(feature = "bench-support")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pointer {
    Off,
    /// Autopilot's chip, a harness host drawing it.
    Chip,
    /// The branch fact, a harness host drawing the block its title takes.
    Fact,
}

/// THE PROBE: `passes` footer passes timed for each kind of frame, after a warm-up.
#[cfg(feature = "bench-support")]
pub(crate) fn footer_pass_cost(passes: usize) -> Vec<PassCost> {
    let kinds: [(&'static str, Option<Change>, Pointer); 6] = [
        ("at rest", None, Pointer::Off),
        ("keystroke", Some(keystroke), Pointer::Off),
        ("streamed row", Some(streamed), Pointer::Off),
        ("spinner tick", Some(spinner), Pointer::Off),
        ("hovered chip", None, Pointer::Chip),
        ("hovered fact", None, Pointer::Fact),
    ];
    let mut out = Vec::with_capacity(kinds.len());
    for (what, step, pointer) in kinds {
        let (mut app, wid, session) = probe_glass();
        let _host = (pointer != Pointer::Off).then(|| {
            let host = hosted(&mut app);
            match pointer {
                Pointer::Chip => app
                    .hover_claude_chip(wid, Some((session, crate::claude_lights::Chip::Autopilot))),
                _ => {
                    app.hover_claude_fact(wid, Some((session, crate::claude_lights::Fact::Branch)))
                }
            }
            host
        });
        let frame = |app: &mut App, i: usize| {
            if let Some(step) = step {
                step(app, session, i);
                refill(app, wid);
            }
        };
        for i in 0..64 {
            frame(&mut app, i);
            let _ = pass(&mut app, wid);
        }
        let mut micros = Vec::with_capacity(passes);
        for i in 64..64 + passes {
            frame(&mut app, i);
            let (plan, route) = app.active_visible_frame_layout(wid).expect("a layout");
            let t = Instant::now();
            let _ = std::hint::black_box(pass_on(&mut app, wid, &plan, route));
            micros.push(t.elapsed().as_secs_f64() * 1e6);
        }
        micros.sort_by(f64::total_cmp);
        let at = |q: f64| micros[((micros.len() - 1) as f64 * q) as usize];
        out.push(PassCost {
            what,
            p50_us: at(0.5),
            p90_us: at(0.9),
            p99_us: at(0.99),
        });
    }
    out
}
