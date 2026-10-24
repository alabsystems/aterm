// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Self-contained HTML renderer for `aterm drive ledger --format html`.

use std::fmt::Write as _;

use super::journal::Noted;
use super::ledger::{Ledger, MarkKind, html_json_str, html_text, stamp, summary_rows, zone};

/// The page.
pub fn render_html(l: &Ledger) -> String {
    let s = l.summary();
    let items = l.items();
    let title = format!("aterm drive ledger — @{}", l.worker);
    let mut out = String::with_capacity(16 * 1024);
    out.push_str("<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str(
        "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; \
         style-src 'unsafe-inline'; script-src 'unsafe-inline'; base-uri 'none'; \
         form-action 'none'\">\n",
    );
    let _ = writeln!(out, "<title>{}</title>", html_text(&title));
    out.push_str(STYLE);
    out.push_str("</head>\n<body class=\"viz-root\">\n<main>\n");
    let _ = writeln!(
        out,
        "<header>\n<h1>aterm drive ledger</h1>\n<p class=\"sub\">worker <b>@{}</b> · manager \
         {} · read {} · times {}{}</p>\n</header>",
        html_text(&l.worker),
        l.manager
            .as_deref()
            .map_or("<b>-</b>".to_string(), |m| format!(
                "<b>@{}</b>",
                html_text(m)
            )),
        html_text(&stamp(l.now_ms, l.tz_offset_s, true)),
        html_text(&zone(l.tz_offset_s)),
        l.since_ms.map_or(String::new(), |t| format!(
            " · since {}",
            html_text(&stamp(t, l.tz_offset_s, true))
        ))
    );

    out.push_str("<section class=\"tiles\" aria-label=\"summary\">\n");
    for (k, v) in summary_rows(l, &s) {
        let _ = writeln!(
            out,
            "<div class=\"tile\"><div class=\"k\">{}</div><div class=\"v\">{}</div></div>",
            html_text(k),
            html_text(&v)
        );
    }
    out.push_str("</section>\n");

    out.push_str("<section aria-label=\"sources\">\n<h2>Sources</h2>\n<ul class=\"sources\">\n");
    for src in &l.sources {
        let _ = writeln!(
            out,
            "<li class=\"{}\"><span class=\"name\">{}</span><span>{}</span></li>",
            if src.ok { "ok" } else { "miss" },
            html_text(src.name),
            html_text(&src.detail)
        );
    }
    out.push_str("</ul>\n</section>\n");

    out.push_str(
        "<section aria-label=\"swimlanes\">\n<h2>Swimlanes</h2>\n\
         <div class=\"controls\">\n\
         <button type=\"button\" id=\"zoom-out\">−</button>\
         <button type=\"button\" id=\"zoom-fit\">fit</button>\
         <button type=\"button\" id=\"zoom-in\">+</button>\
         <span class=\"hint\">hover a mark for the whole line</span>\n</div>\n\
         <div class=\"plot\" id=\"plot\"></div>\n\
         <div class=\"tip\" id=\"tip\" hidden></div>\n\
         <noscript><p class=\"hint\">The swimlanes are drawn by the page's own script; \
         every row is in the table below either way.</p></noscript>\n\
         <ul class=\"legend\">\n",
    );
    for (kind, cls, what) in legend() {
        let _ = writeln!(
            out,
            "<li data-kind=\"{kind}\"><span class=\"swatch {cls}\"></span>{}</li>",
            html_text(what)
        );
    }
    out.push_str("</ul>\n</section>\n");

    out.push_str(
        "<section aria-label=\"timeline\">\n<h2>Timeline</h2>\n<div class=\"scroll\">\n\
         <table>\n<thead><tr><th>time</th><th>lane</th><th>what</th>\
         <th>duration/latency</th></tr></thead>\n<tbody>\n",
    );
    let dated = items
        .iter()
        .filter_map(|i| i.t)
        .fold((i64::MAX, i64::MIN), |(lo, hi), t| (lo.min(t), hi.max(t)));
    let dated = dated.0 != i64::MAX && dated.1 - dated.0 > 12 * 3600 * 1000;
    for i in &items {
        let _ = writeln!(
            out,
            "<tr class=\"lane-{}\"><td class=\"t\">{}</td><td>{}</td><td>{}</td>\
             <td class=\"d\">{}</td></tr>",
            i.lane.name(),
            html_text(&i.t.map_or_else(|| "-".to_string(), |t| stamp(t, l.tz_offset_s, dated))),
            html_text(i.lane.name()),
            html_text(&i.full),
            html_text(&i.dur)
        );
    }
    out.push_str("</tbody>\n</table>\n</div>\n</section>\n</main>\n");

    out.push_str("<script type=\"application/json\" id=\"ledger-data\">\n");
    out.push_str(&data_json(l, &items));
    out.push_str("\n</script>\n<script>\n");
    out.push_str(SCRIPT);
    out.push_str("</script>\n</body>\n</html>\n");
    out
}

fn data_json(l: &Ledger, items: &[super::ledger::Item]) -> String {
    let placed: Vec<&super::ledger::Item> = items.iter().filter(|i| i.t.is_some()).collect();
    let t0 = placed.iter().filter_map(|i| i.t).min().unwrap_or(0);
    let t1 = placed
        .iter()
        .flat_map(|i| [i.t, i.end, i.long_end])
        .flatten()
        .max()
        .unwrap_or(t0 + 1);
    let mut out = format!(
        "{{\"t0\":{t0},\"t1\":{t1},\"tz\":{},\"aligned\":{},\"items\":[",
        l.tz_offset_s, l.aligned
    );
    for (n, i) in placed.iter().enumerate() {
        if n > 0 {
            out.push(',');
        }
        let num = |v: Option<i64>| v.map_or("null".to_string(), |v| v.to_string());
        let _ = write!(
            out,
            "{{\"t\":{},\"end\":{},\"long\":{},\"lane\":{},\"kind\":{},\"cls\":{},\"what\":{},\
             \"dur\":{}}}",
            num(i.t),
            num(i.end),
            num(i.long_end),
            html_json_str(i.lane.name()),
            html_json_str(i.mark.name()),
            html_json_str(swatch(i.mark)),
            html_json_str(&i.full),
            html_json_str(&i.dur)
        );
    }
    out.push_str("]}");
    out
}

/// The legend: one row per mark ([`MarkKind::ALL`]), and the reply's `working` bar after
/// the reply's own: the row's word, its swatch and what it marks.
pub(super) fn legend() -> Vec<(&'static str, &'static str, &'static str)> {
    let mut rows = Vec::with_capacity(MarkKind::ALL.len() + 1);
    for m in MarkKind::ALL {
        rows.push((m.name(), swatch(m), marks(m)));
        if m == MarkKind::Reply {
            rows.push((
                "working",
                "working",
                "worker: on to the EVENT that ended the reply",
            ));
        }
    }
    rows
}

/// The swatch class a mark is drawn in (a `.swatch.<class>` rule in [`STYLE`]).
pub(super) const fn swatch(m: MarkKind) -> &'static str {
    match m {
        MarkKind::Turn => "turn",
        MarkKind::Reply => "reply",
        MarkKind::MailIn | MarkKind::MailOut => "mail",
        MarkKind::Limited | MarkKind::LimitHandled | MarkKind::Timeout | MarkKind::Exit => "bad",
        MarkKind::Reconnect | MarkKind::Escalated | MarkKind::Needs | MarkKind::Unverified => {
            "warn"
        }
        MarkKind::Unread => "unread",
        MarkKind::Noted(w) => noted_swatch(w),
        MarkKind::Idle
        | MarkKind::Question
        | MarkKind::Prompt
        | MarkKind::Survey
        | MarkKind::Context
        | MarkKind::Busy
        | MarkKind::Approved
        | MarkKind::Dismissed
        | MarkKind::Mail
        | MarkKind::Extend
        | MarkKind::Cleared
        | MarkKind::Continued
        | MarkKind::Typed
        | MarkKind::Restarted
        | MarkKind::Waiting
        | MarkKind::Skipped
        | MarkKind::Unproven
        | MarkKind::Dialog
        | MarkKind::Declined
        | MarkKind::Chose
        | MarkKind::Exception
        | MarkKind::Probed
        | MarkKind::Reset
        | MarkKind::Hold
        | MarkKind::Occupancy
        | MarkKind::Probe => "event",
    }
}

/// The swatch of a [`Noted`] word's mark: `bad` where the loop could not act or the session
/// stopped answering, `warn` where it says something the person may want to look at.
const fn noted_swatch(w: Noted) -> &'static str {
    match w {
        Noted::Halted
        | Noted::Refused
        | Noted::RestartRefused
        | Noted::Stalled
        | Noted::Stuck
        | Noted::Frozen
        | Noted::Signalled => "bad",
        Noted::Warn
        | Noted::StallWarned
        | Noted::Memory
        | Noted::Abandoned
        | Noted::Claim
        | Noted::RepoUnread
        | Noted::Watching => "warn",
        Noted::Done
        | Noted::Left
        | Noted::Parked
        | Noted::Unparked
        | Noted::Unhalted
        | Noted::Held
        | Noted::HoldEnded
        | Noted::Adopted
        | Noted::Kept
        | Noted::Claimed
        | Noted::NotEscalated
        | Noted::Task
        | Noted::Fact
        | Noted::Launch
        | Noted::Host
        | Noted::Deferred
        | Noted::Thawed
        | Noted::Note
        | Noted::Sleep => "event",
    }
}

/// What a [`Noted`] word's legend row says it marks.
const fn noted_marks(w: Noted) -> &'static str {
    match w {
        Noted::Done => "watcher: a DONE line, the task's end the loop read",
        Noted::Left => "watcher: a LEFT line, a turn left to the person",
        Noted::Parked => "watcher: a PARKED line, hands off while Autopilot is off",
        Noted::Unparked => "watcher: an UNPARKED line, Autopilot back on",
        Noted::Halted => "watcher: a HALTED line, the session on hold: the loop waits",
        Noted::Unhalted => "watcher: an UNHALTED line, the hold lifted",
        Noted::Refused => "watcher: a REFUSED line, a request refused: the loop backs off",
        Noted::Held => "watcher: a HELD line, hands off while a person or driver types",
        Noted::HoldEnded => "watcher: a HOLD-ENDED line, a hold ended before the vendor's deny",
        Noted::Adopted => "watcher: an ADOPTED line, a badge a previous loop left",
        Noted::Kept => "watcher: a KEPT line, a badge kept for the next loop",
        Noted::Watching => "watcher: a WATCHING line, another supervisor answers",
        Noted::Claim => "watcher: a CLAIM line, no claim: the loop acts without one",
        Noted::Claimed => "watcher: a CLAIMED line, the session's claim taken",
        Noted::NotEscalated => "watcher: a NOT ESCALATED line, a notice already stands",
        Noted::Warn => "watcher: a WARN line, a warning the loop says aloud",
        Noted::Task => "watcher: a TASK line, the session's task mark",
        Noted::Fact => "watcher: a FACT line, a fact for the agent's next prompt",
        Noted::Launch => "watcher: a LAUNCH line, a box the person's own Enter launched",
        Noted::Host => "watcher: a HOST line, a step of the window's harness",
        Noted::Deferred => "watcher: a DEFERRED line, a restart put off",
        Noted::Memory => "watcher: a MEMORY line, the agent over its memory measure",
        Noted::Stalled => "watcher: a STALLED line, a stalled call told to the window",
        Noted::Stuck => "watcher: a STUCK line, a screen no reader names",
        Noted::Frozen => "watcher: a FROZEN line, the worker not reading its input",
        Noted::Thawed => "watcher: a THAWED line, the worker reading again",
        Noted::StallWarned => "watcher: a STALL-WARNED line, a stall the loop warned of",
        Noted::Signalled => "watcher: a SIGNALLED line, a signal sent to a stalled worker",
        Noted::RestartRefused => "watcher: a RESTART-REFUSED line, a restart the host refused",
        Noted::Note => "watcher: a NOTE line, a reading the loop notes",
        Noted::Sleep => "watcher: a SLEEP line, a sleep the loop could not count",
        Noted::Abandoned => "watcher: an ABANDONED line, a survey left under typed text",
        Noted::RepoUnread => "watcher: a REPO UNREAD line, a repository probe did not finish",
    }
}

/// What a mark's legend row says it marks.
const fn marks(m: MarkKind) -> &'static str {
    match m {
        MarkKind::Noted(w) => noted_marks(w),
        MarkKind::Turn => "manager: a turn typed into the worker",
        MarkKind::Reply => "worker: the turn verb, from typing to settled",
        MarkKind::Idle => "watcher: an EVENT idle, the worker stopped on words",
        MarkKind::Question => "watcher: an EVENT question, the worker asked",
        MarkKind::Prompt => "watcher: an EVENT prompt, an approval box",
        MarkKind::Limited => "watcher: an EVENT limited, a usage or rate limit notice",
        MarkKind::Survey => "watcher: an EVENT survey, a rating survey",
        MarkKind::Context => "watcher: an EVENT context or compacted",
        MarkKind::Busy => "watcher: an EVENT busy",
        MarkKind::Approved => "watcher: an APPROVED line, a box the loop approved",
        MarkKind::Dismissed => "watcher: a DISMISSED line",
        MarkKind::Reconnect => "watcher: a RECONNECT or RECONNECTED line",
        MarkKind::Timeout => "watcher: a TIMEOUT line",
        MarkKind::Exit => "watcher: an EXIT line",
        MarkKind::Mail => "watcher: a MAIL line",
        MarkKind::Extend => "watcher: an EXTEND line",
        MarkKind::Escalated => "watcher: an ESCALATED line, an ask put on the person's screen",
        MarkKind::Cleared => "watcher: a CLEARED line, a badge or an exception ended",
        MarkKind::Continued => "watcher: a CONTINUED line, a carry-on the loop typed",
        MarkKind::Typed => "watcher: a TYPED line",
        MarkKind::Restarted => "watcher: a RESTARTED line",
        MarkKind::Waiting => "watcher: a WAITING line",
        MarkKind::Skipped => "watcher: a SKIPPED line",
        MarkKind::Unverified => "watcher: an UNVERIFIED line, a submit not seen",
        MarkKind::LimitHandled => "watcher: a LIMITED line, a limit notice the loop handled",
        MarkKind::Unproven => "watcher: an UNPROVEN line",
        MarkKind::Dialog => "watcher: a DIALOG line",
        MarkKind::Declined => "watcher: a DECLINED line",
        MarkKind::Chose => "watcher: a CHOSE line, a question the loop answered",
        MarkKind::Exception => "watcher: an EXCEPTION line",
        MarkKind::Probed => "watcher: a PROBED line",
        MarkKind::Reset => "watcher: a RESET line",
        MarkKind::Needs => "watcher: a NEEDS line, the worker's NEEDS YOU on the person's screen",
        MarkKind::Hold => "watcher: a HOLD line",
        MarkKind::Occupancy => "watcher: an OCCUPANCY line",
        MarkKind::Probe => "watcher: a PROBE line",
        MarkKind::MailIn => "fabric: a message from the worker",
        MarkKind::MailOut => "fabric: a message to the worker",
        MarkKind::Unread => "watcher: a line this reader does not know",
    }
}

pub(super) const STYLE: &str = r#"<style>
.viz-root {
  color-scheme: light;
  --surface-1: #fcfcfb;
  --plane: #f9f9f7;
  --line: #dcdcd6;
  --text-primary: #0b0b0b;
  --text-secondary: #52514e;
  --text-muted: #77766f;
  --manager: #2a78d6;
  --worker: #1baf7a;
  --worker-soft: #b7e3d2;
  --watcher: #eb6834;
  --fabric: #4a3aa7;
  --warning: #fab219;
  --critical: #d03b3b;
}
@media (prefers-color-scheme: dark) {
  :root:where(:not([data-theme="light"])) .viz-root {
    color-scheme: dark;
    --surface-1: #1a1a19;
    --plane: #0d0d0d;
    --line: #3a3a37;
    --text-primary: #ffffff;
    --text-secondary: #c3c2b7;
    --text-muted: #94938a;
    --manager: #3987e5;
    --worker: #199e70;
    --worker-soft: #1b5344;
    --watcher: #d95926;
    --fabric: #9085e9;
    --warning: #fab219;
    --critical: #d03b3b;
  }
}
:root[data-theme="dark"] .viz-root {
  color-scheme: dark;
  --surface-1: #1a1a19;
  --plane: #0d0d0d;
  --line: #3a3a37;
  --text-primary: #ffffff;
  --text-secondary: #c3c2b7;
  --text-muted: #94938a;
  --manager: #3987e5;
  --worker: #199e70;
  --worker-soft: #1b5344;
  --watcher: #d95926;
  --fabric: #9085e9;
  --warning: #fab219;
  --critical: #d03b3b;
}
* { box-sizing: border-box; }
body.viz-root {
  margin: 0;
  background: var(--plane);
  color: var(--text-primary);
  font: 14px/1.45 ui-sans-serif, -apple-system, "Segoe UI", Helvetica, Arial, sans-serif;
}
main { max-width: 1400px; margin: 0 auto; padding: 24px 16px 64px; }
h1 { font-size: 20px; margin: 0 0 4px; }
h2 { font-size: 13px; letter-spacing: .06em; text-transform: uppercase;
     color: var(--text-secondary); margin: 28px 0 8px; font-weight: 600; }
.sub { margin: 0; color: var(--text-secondary); }
.sub b { color: var(--text-primary); font-weight: 600; }
.tiles { display: grid; gap: 8px; margin-top: 20px;
         grid-template-columns: repeat(auto-fit, minmax(240px, 1fr)); }
.tile { background: var(--surface-1); border: 1px solid var(--line); border-radius: 8px;
        padding: 10px 12px; }
.tile .k { color: var(--text-muted); font-size: 12px; text-transform: uppercase;
           letter-spacing: .05em; }
.tile .v { margin-top: 2px; font-variant-numeric: tabular-nums; }
ul.sources { list-style: none; margin: 0; padding: 0; }
ul.sources li { display: flex; gap: 10px; padding: 3px 0; color: var(--text-secondary); }
ul.sources .name { min-width: 84px; color: var(--text-primary); font-weight: 600; }
ul.sources li.miss .name::after { content: " ✗"; color: var(--critical); }
.controls { display: flex; align-items: center; gap: 6px; margin-bottom: 6px; }
.controls button { font: inherit; color: var(--text-primary); background: var(--surface-1);
                   border: 1px solid var(--line); border-radius: 6px; padding: 2px 10px;
                   cursor: pointer; }
.hint { color: var(--text-muted); font-size: 12px; margin-left: 6px; }
.plot { overflow-x: auto; background: var(--surface-1); border: 1px solid var(--line);
        border-radius: 8px; }
.plot svg { display: block; }
.tip { position: fixed; z-index: 9; max-width: 520px; pointer-events: none;
       background: var(--surface-1); color: var(--text-primary);
       border: 1px solid var(--line); border-radius: 6px; padding: 6px 8px;
       box-shadow: 0 2px 10px rgba(0,0,0,.18); font-size: 12px; white-space: pre-wrap; }
.tip[hidden] { display: none; }
ul.legend { list-style: none; display: flex; flex-wrap: wrap; gap: 4px 18px;
            margin: 8px 0 0; padding: 0; color: var(--text-secondary); font-size: 12px; }
ul.legend li { display: flex; align-items: center; gap: 6px; }
.swatch { width: 10px; height: 10px; border-radius: 2px; display: inline-block; }
.swatch.turn { background: var(--manager); clip-path: polygon(50% 0, 100% 100%, 0 100%); }
.swatch.reply { background: var(--worker); }
.swatch.working { background: var(--worker-soft); }
.swatch.event { background: var(--watcher); border-radius: 50%; }
.swatch.warn { background: var(--warning); border-radius: 50%; }
.swatch.bad { background: var(--critical); border-radius: 50%; }
.swatch.mail { background: var(--fabric); transform: rotate(45deg); }
.swatch.unread { background: var(--text-muted); border-radius: 50%; }
.scroll { overflow-x: auto; }
table { border-collapse: collapse; width: 100%; font-size: 13px; }
th, td { text-align: left; vertical-align: top; padding: 4px 10px 4px 0;
         border-bottom: 1px solid var(--line); }
th { color: var(--text-muted); font-size: 12px; text-transform: uppercase;
     letter-spacing: .05em; font-weight: 600; }
td.t, td.d { font-variant-numeric: tabular-nums; white-space: nowrap; }
td.d { color: var(--text-secondary); }
tr.lane-manager td:nth-child(2) { color: var(--manager); }
tr.lane-worker td:nth-child(2) { color: var(--worker); }
tr.lane-watcher td:nth-child(2) { color: var(--watcher); }
tr.lane-fabric td:nth-child(2) { color: var(--fabric); }
</style>
"#;

pub(super) const SCRIPT: &str = r#"(function () {
  var el = document.getElementById("ledger-data");
  var data = JSON.parse(el.textContent);
  var plot = document.getElementById("plot");
  var tip = document.getElementById("tip");
  var LANES = ["manager", "watcher", "worker", "fabric"];
  var LABEL = { manager: "manager", watcher: "watcher", worker: "worker", fabric: "fabric" };
  var SVG = "http://www.w3.org/2000/svg";
  var PAD = { left: 92, right: 24, top: 26, bottom: 8 };
  var ROW = 46;
  var span = Math.max(1, data.t1 - data.t0);
  var scale = 1;               // multiples of the fit width
  var steps = [1e3, 5e3, 15e3, 6e4, 3e5, 9e5, 36e5, 108e5, 216e5, 432e5, 864e5];

  function make(name, attrs) {
    var n = document.createElementNS(SVG, name);
    for (var k in attrs) { if (attrs[k] !== null) n.setAttribute(k, attrs[k]); }
    return n;
  }
  function clock(ms) {
    var s = Math.floor(ms / 1000) + data.tz;
    var d = ((s % 86400) + 86400) % 86400;
    function two(n) { return (n < 10 ? "0" : "") + n; }
    return two(Math.floor(d / 3600)) + ":" + two(Math.floor((d % 3600) / 60)) + ":" + two(d % 60);
  }
  function tell(node, text) {
    node.appendChild(make("title", {})).textContent = text;
    node.addEventListener("mousemove", function (e) {
      tip.textContent = text;
      tip.hidden = false;
      var x = Math.min(e.clientX + 14, window.innerWidth - 540);
      tip.style.left = Math.max(8, x) + "px";
      tip.style.top = Math.min(e.clientY + 16, window.innerHeight - 90) + "px";
    });
    node.addEventListener("mouseleave", function () { tip.hidden = true; });
  }
  function colour(cls) {
    if (cls === "bad") { return "var(--critical)"; }
    if (cls === "warn") { return "var(--warning)"; }
    if (cls === "turn") { return "var(--manager)"; }
    if (cls === "mail") { return "var(--fabric)"; }
    if (cls === "reply") { return "var(--worker)"; }
    if (cls === "unread") { return "var(--text-muted)"; }
    return "var(--watcher)";
  }

  function draw() {
    var wide = Math.max(plot.clientWidth || 900, 320);
    var inner = Math.max(240, (wide - PAD.left - PAD.right) * scale);
    var width = inner + PAD.left + PAD.right;
    var height = PAD.top + LANES.length * ROW + PAD.bottom;
    var x = function (t) { return PAD.left + ((t - data.t0) / span) * inner; };
    var svg = make("svg", { width: width, height: height,
                            viewBox: "0 0 " + width + " " + height, role: "img",
                            "aria-label": "the manager loop on a time axis" });

    // The lanes.
    LANES.forEach(function (lane, i) {
      var y = PAD.top + i * ROW;
      svg.appendChild(make("rect", { x: 0, y: y, width: width, height: ROW,
                                     fill: i % 2 ? "var(--plane)" : "var(--surface-1)" }));
      var label = make("text", { x: 12, y: y + ROW / 2 + 4, fill: "var(--" + lane + ")",
                                 "font-size": 12, "font-weight": 600 });
      label.textContent = LABEL[lane];
      svg.appendChild(label);
      svg.appendChild(make("line", { x1: PAD.left, y1: y + ROW, x2: width - PAD.right,
                                     y2: y + ROW, stroke: "var(--line)", "stroke-width": 1 }));
    });

    // The time axis: the first step that leaves the ticks 90 px apart.
    var step = steps[steps.length - 1];
    for (var s = 0; s < steps.length; s++) {
      if ((steps[s] / span) * inner >= 90) { step = steps[s]; break; }
    }
    var first = Math.ceil(data.t0 / step) * step;
    for (var t = first; t <= data.t1; t += step) {
      svg.appendChild(make("line", { x1: x(t), y1: PAD.top, x2: x(t),
                                     y2: height - PAD.bottom, stroke: "var(--line)",
                                     "stroke-width": 1, "stroke-dasharray": "2 4" }));
      var tick = make("text", { x: x(t), y: 16, fill: "var(--text-muted)", "font-size": 11,
                                "text-anchor": "middle" });
      tick.textContent = clock(t);
      svg.appendChild(tick);
    }

    // The marks.
    data.items.forEach(function (it) {
      var row = LANES.indexOf(it.lane);
      if (row < 0) { return; }
      var mid = PAD.top + row * ROW + ROW / 2;
      var text = clock(it.t) + "  " + it.lane + "\n" + it.what +
                 (it.dur && it.dur !== "-" ? "\n" + it.dur : "");
      var node;
      if (it.kind === "reply") {
        var g = make("g", {});
        if (it.long !== null && it.long > (it.end === null ? it.t : it.end)) {
          g.appendChild(make("rect", { x: x(it.t), y: mid - 5,
                                       width: Math.max(2, x(it.long) - x(it.t)), height: 10,
                                       rx: 4, fill: "var(--worker-soft)" }));
        }
        var end = it.end === null ? it.t : it.end;
        g.appendChild(make("rect", { x: x(it.t), y: mid - 5,
                                     width: Math.max(3, x(end) - x(it.t)), height: 10, rx: 4,
                                     fill: "var(--worker)" }));
        node = g;
      } else if (it.kind === "turn") {
        node = make("polygon", { points: (x(it.t) - 6) + "," + (mid + 5) + " " +
                                         (x(it.t) + 6) + "," + (mid + 5) + " " +
                                         x(it.t) + "," + (mid - 6),
                                 fill: colour(it.cls), stroke: "var(--surface-1)",
                                 "stroke-width": 2 });
      } else if (it.kind === "mail-in" || it.kind === "mail-out") {
        node = make("rect", { x: x(it.t) - 5, y: mid - 5, width: 10, height: 10,
                              transform: "rotate(45 " + x(it.t) + " " + mid + ")",
                              fill: it.kind === "mail-in" ? "var(--fabric)" : "var(--surface-1)",
                              stroke: "var(--fabric)", "stroke-width": 2 });
      } else {
        node = make("circle", { cx: x(it.t), cy: mid, r: 5, fill: colour(it.cls),
                                stroke: "var(--surface-1)", "stroke-width": 2 });
      }
      tell(node, text);
      svg.appendChild(node);
    });

    plot.textContent = "";
    plot.appendChild(svg);
  }

  function zoom(by) { scale = Math.min(64, Math.max(1, scale * by)); draw(); }
  document.getElementById("zoom-in").addEventListener("click", function () { zoom(2); });
  document.getElementById("zoom-out").addEventListener("click", function () { zoom(0.5); });
  document.getElementById("zoom-fit").addEventListener("click", function () { scale = 1; draw(); });
  window.addEventListener("resize", draw);
  draw();
})();
"#;

/// Checks that `html` contains no external fetches and has a strict `Content-Security-Policy`.
#[cfg(test)]
pub(super) fn is_self_contained(html: &str) -> Result<(), String> {
    for bad in [
        "src=\"http",
        "href=\"http",
        "url(http",
        "@import",
        "<iframe",
        "<img",
        "fetch(",
        "XMLHttpRequest",
    ] {
        if html.contains(bad) {
            return Err(format!("the page reaches for {bad}"));
        }
    }
    if !html.contains("default-src 'none'") {
        return Err("no content policy".to_string());
    }
    Ok(())
}
