// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `aterm drive ledger --format html`: ONE self-contained page — the summary
//! on top, then three swimlanes (manager, watcher, worker) and the fabric's
//! mail on a time axis, then every row as a table.
//!
//! No network: the style and the script are inline, there is no image and no
//! font to fetch, and a `Content-Security-Policy` meta says `default-src
//! 'none'` so a URL that appears in a worker's own words cannot become a
//! request. Every string from the worker, the journal or the bus is escaped —
//! into the HTML as text ([`html_text`]) and into the one embedded JSON
//! document with `<`, `>` and `&` as `\u` escapes ([`html_json_str`]), so
//! neither a `</script>` in a transcript nor a quote in a command can close a
//! tag.
//!
//! The marks: a turn is a BAR in the worker lane, from when the `turn` verb
//! started to where it settled, with a lighter bar on to the EVENT that ended
//! the worker's reply (the journal's, when there is one); the manager's turns,
//! the watcher's lines and the mail are marks on their own lanes, one shape
//! per lane and one hue per lane, with a status colour for what went wrong (a
//! limit notice, an EXIT, a reconnect). Hovering any of them shows the whole
//! line, which the table below carries too — the chart is never the only way
//! to read a row.
//!
//! The palette is the validated categorical default (blue, orange, aqua,
//! violet) with the fixed status pair, stepped for each surface and declared
//! under both the media query and the theme attribute, so the page reads in
//! light and dark alike.

use std::fmt::Write as _;

use super::ledger::{Ledger, html_json_str, html_text, stamp, summary_rows, zone};

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

    // The summary, as tiles.
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

    // What was read.
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

    // The swimlanes.
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
    for (cls, what) in LEGEND {
        let _ = writeln!(
            out,
            "<li><span class=\"swatch {cls}\"></span>{}</li>",
            html_text(what)
        );
    }
    out.push_str("</ul>\n</section>\n");

    // Every row.
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
            html_text(&match i.t {
                None => "-".to_string(),
                Some(t) => format!(
                    "{}{}",
                    if l.aligned { "" } else { "~" },
                    stamp(t, l.tz_offset_s, dated)
                ),
            }),
            html_text(i.lane.name()),
            html_text(&i.full),
            html_text(&i.dur)
        );
    }
    out.push_str("</tbody>\n</table>\n</div>\n</section>\n</main>\n");

    // The data the script draws, and the script.
    out.push_str("<script type=\"application/json\" id=\"ledger-data\">\n");
    out.push_str(&data_json(l, &items));
    out.push_str("\n</script>\n<script>\n");
    out.push_str(SCRIPT);
    out.push_str("</script>\n</body>\n</html>\n");
    out
}

/// The lanes, top to bottom, and the items on them — one JSON object.
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
            "{{\"t\":{},\"end\":{},\"long\":{},\"lane\":{},\"kind\":{},\"what\":{},\"dur\":{}}}",
            num(i.t),
            num(i.end),
            num(i.long_end),
            html_json_str(i.lane.name()),
            html_json_str(i.mark.name()),
            html_json_str(&i.full),
            html_json_str(&i.dur)
        );
    }
    out.push_str("]}");
    out
}

/// The legend: one row per mark the page draws.
const LEGEND: &[(&str, &str)] = &[
    ("turn", "manager: a turn typed into the worker"),
    ("reply", "worker: the turn verb, from typing to settled"),
    ("working", "worker: on to the EVENT that ended the reply"),
    ("event", "watcher: an EVENT, APPROVED or DISMISSED line"),
    ("warn", "watcher: a RECONNECT line"),
    ("bad", "watcher: a limit notice, a TIMEOUT or an EXIT"),
    ("mail", "fabric: a message to or from the worker"),
];

/// The page's own style: the validated categorical palette (blue, orange,
/// aqua, violet) and the fixed status pair, both surfaces, declared under the
/// media query and the theme attribute alike.
const STYLE: &str = r#"<style>
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

/// The page's own script: it draws the lanes from the embedded data, and
/// nothing else. No fetch, no import, no timer.
const SCRIPT: &str = r#"(function () {
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
  function colour(kind) {
    if (kind === "limited" || kind === "end") { return "var(--critical)"; }
    if (kind === "reconnect") { return "var(--warning)"; }
    if (kind === "turn") { return "var(--manager)"; }
    if (kind === "mail-in" || kind === "mail-out") { return "var(--fabric)"; }
    if (kind === "reply") { return "var(--worker)"; }
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
                                 fill: colour(it.kind), stroke: "var(--surface-1)",
                                 "stroke-width": 2 });
      } else if (it.kind === "mail-in" || it.kind === "mail-out") {
        node = make("rect", { x: x(it.t) - 5, y: mid - 5, width: 10, height: 10,
                              transform: "rotate(45 " + x(it.t) + " " + mid + ")",
                              fill: it.kind === "mail-in" ? "var(--fabric)" : "var(--surface-1)",
                              stroke: "var(--fabric)", "stroke-width": 2 });
      } else {
        node = make("circle", { cx: x(it.t), cy: mid, r: 5, fill: colour(it.kind),
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

/// Whether `html` is one self-contained file: nothing it would fetch, and a
/// policy that forbids a fetch even if a worker's own words held a URL. (The
/// SVG namespace is a name, not a request: `createElementNS` never opens it.)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every swatch the legend names has a style, and the style and script
    /// fetch nothing.
    #[test]
    fn the_legend_names_every_shape_the_script_draws() {
        for (cls, _) in LEGEND {
            assert!(
                STYLE.contains(&format!(".swatch.{cls} ")),
                "the legend's {cls} swatch has no style"
            );
        }
        for part in [STYLE, SCRIPT] {
            assert_eq!(
                is_self_contained(&format!("default-src 'none'{part}")),
                Ok(())
            );
        }
    }
}
