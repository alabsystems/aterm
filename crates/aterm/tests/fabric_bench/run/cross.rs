// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE CROSS-NODE ARM (B11, leg e): two headless aterms, each with its own
//! `aterm link serve`, on one broker in this process (`harness::Fleet`),
//! opened with aterm's options for the run's tier. Session A posts to a
//! session on its own node (the control), to a bare `@s-<sid>` the other node
//! hosts, and to the same session as `@s-<sid>@n-<node>`: one at a time, then
//! in bursts. Both bridges run with the wait tally on, so the sender's
//! per-post work — the routing reads a post to another node costs — is
//! counted beside the times. Measures, never asserts a floor.

use std::path::PathBuf;
use std::time::Duration;

use aterm_json::{Value, json};
use aterm_link::ctl::Ctl;
use aterm_link::wait_tally;

use super::ask::{
    HEAD_U, Mark, Spent, await_tally_where, blocks, flushes_per_post, json_u, row_u, settled,
    steps_json, steps_rows,
};
use super::{
    BURST_WINDOW, CloseOnDrop, Credit, FLEET, Ruler, await_note, calibrate, consume, list_since,
    ms, ms_opt, newest_id, opts_for, post, sampled_on, served_line_of, tally, yardstick,
    yardstick_json, yardstick_rows,
};
use crate::clock::{Clock, Heads};
use crate::harness::{Fleet, Node, until};
use crate::spec::{Log, Spec};
use crate::stats;

/// How A addresses the recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Form {
    /// A second session on A's own node: the control.
    Local,
    /// `@s-<sid>`, a session the other node hosts.
    Bare,
    /// `@s-<sid>@n-<node>`, the same session with its node named.
    AtNode,
}

impl Form {
    const ALL: [Form; 3] = [Form::Local, Form::Bare, Form::AtNode];

    const fn word(self) -> &'static str {
        match self {
            Form::Local => "local",
            Form::Bare => "bare",
            Form::AtNode => "at-node",
        }
    }

    const fn says(self) -> &'static str {
        match self {
            Form::Local => "`@s-<sid>` on A's own node (the control)",
            Form::Bare => "a bare `@s-<sid>` on the other node",
            Form::AtNode => "`@s-<sid>@n-<node>` on the other node",
        }
    }
}

/// The two nodes and the three sessions the arm posts between.
struct Ends<'a> {
    fleet: &'a Fleet,
    a: &'a Node,
    b: &'a Node,
    /// A, the sender, and A's neighbour on its own node.
    sa: String,
    sa2: String,
    /// The session on the other node.
    sb: String,
    clock: Clock,
    a_tally: PathBuf,
    a_pid: i32,
    b_tally: PathBuf,
    b_pid: i32,
}

impl Ends<'_> {
    /// What A writes after `to=@`.
    fn to(&self, form: Form) -> String {
        match form {
            Form::Local => self.sa2.clone(),
            Form::Bare => self.sb.clone(),
            Form::AtNode => format!("{}@{}", self.sb, self.b.node),
        }
    }

    /// Whom it reaches: its node and its sid.
    fn recipient(&self, form: Form) -> (&Node, &str) {
        match form {
            Form::Local => (self.a, &self.sa2),
            Form::Bare | Form::AtNode => (self.b, &self.sb),
        }
    }

    /// Wait for the recipient's bridge to commit through `off`, then for A's
    /// tally past `sent` posts retired (and through `off` too when A's
    /// bridge is the recipient's): A's tally and the broker's flushes.
    fn settle(&self, form: Form, off: u64, sent: u64, what: &str) -> Mark {
        if form != Form::Local {
            await_tally_where(&self.b_tally, self.b_pid, what, settled(off, 0));
        }
        let through = if form == Form::Local { off } else { 0 };
        let tally = await_tally_where(&self.a_tally, self.a_pid, what, settled(through, sent));
        Mark {
            tally,
            flushes: self.fleet.broker().flush_counts(),
        }
    }
}

/// One message timed one at a time.
#[derive(Debug, Clone, Copy)]
struct Lat {
    t_call: u64,
    t_ret: u64,
    t_seen: u64,
    off: u64,
}

/// One burst: when it started, each post's `OK`, each message's arrival.
struct Burst {
    t0: u64,
    rets: Vec<u64>,
    seen: Vec<(u64, u64)>,
}

/// Each form's numbers, by [`Form::ALL`]'s index.
#[derive(Default)]
struct ByForm {
    lat: [Vec<Lat>; 3],
    lat_spent: [Spent; 3],
    bursts: [Vec<Burst>; 3],
    burst_spent: [Spent; 3],
}

/// The receivers' `since` cursors, by node: A's neighbour and the other node's session.
struct Cursors {
    sa2: u64,
    sb: u64,
}

impl Cursors {
    fn of(&mut self, form: Form) -> &mut u64 {
        match form {
            Form::Local => &mut self.sa2,
            Form::Bare | Form::AtNode => &mut self.sb,
        }
    }
}

fn one(
    e: &Ends,
    form: Form,
    (ctl_a, ctl_b): (&mut Ctl, &mut Ctl),
    since: &mut Cursors,
    text: &str,
    sent: &mut u64,
) -> (Lat, Mark) {
    let (t_call, t_ret) = post(ctl_a, e.clock, &e.sa, &e.to(form), text);
    *sent += 1;
    let (_, rsid) = e.recipient(form);
    let rctl = if form == Form::Local { ctl_a } else { ctl_b };
    let cursor = since.of(form);
    let (woke, t_seen) =
        await_note(rctl, e.clock, rsid, *cursor).unwrap_or_else(|err| panic!("`{text}`: {err}"));
    let rows = list_since(rctl, rsid, *cursor);
    assert!(
        rows.len() == 1 && rows[0].text == text && rows[0].id == woke,
        "`{text}`: @{rsid} woke at row {woke} and listed {rows:?}"
    );
    *cursor = woke;
    let off = rows[0].off;
    let mark = e.settle(form, off, *sent, &format!("`{text}` committed"));
    (
        Lat {
            t_call,
            t_ret,
            t_seen,
            off,
        },
        mark,
    )
}

fn burst(
    e: &Ends,
    form: Form,
    n: usize,
    since: &mut Cursors,
    prefix: &str,
    sent: &mut u64,
) -> (Burst, Mark) {
    let (node, rsid) = e.recipient(form);
    let to = e.to(form);
    let cursor = *since.of(form);
    let credit = Credit::new(BURST_WINDOW);
    let (t0, rets, consumed) = std::thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let _close = CloseOnDrop(&credit);
            let mut ctl = node.ctl();
            consume(&mut ctl, e.clock, rsid, cursor, prefix, n, Some(&credit))
        });
        let mut ctl = e.a.ctl();
        let t0 = e.clock.us();
        let mut rets = Vec::with_capacity(n);
        for j in 0..n {
            if !credit.acquire(j) {
                break;
            }
            rets.push(post(&mut ctl, e.clock, &e.sa, &to, &format!("{prefix}{j}")).1);
        }
        let consumed = reader
            .join()
            .unwrap_or_else(|err| std::panic::resume_unwind(err));
        (t0, rets, consumed)
    });
    let consumed = consumed.unwrap_or_else(|err| panic!("the {} burst: {err}", form.word()));
    let seen = tally(&consumed.seen, n).unwrap_or_else(|err| {
        panic!(
            "the {} burst: every message is seen exactly once, but {err}",
            form.word()
        )
    });
    *since.of(form) = consumed.since;
    *sent += rets.len() as u64;
    let last = seen.iter().map(|s| s.off).max().unwrap_or(0);
    let mark = e.settle(
        form,
        last,
        *sent,
        &format!("the {} burst committed", form.word()),
    );
    (
        Burst {
            t0,
            rets,
            seen: seen.iter().map(|s| (s.off, s.t)).collect(),
        },
        mark,
    )
}

/// Spawn one more session on `node`: its sid.
fn spawn_on(node: &Node) -> String {
    let reply = node.verb("spawn");
    assert!(reply.ok(), "spawn: {}", reply.header());
    reply
        .header()
        .split_whitespace()
        .nth(1)
        .expect("spawn answers OK <sid>")
        .to_string()
}

/// Wait for `sid`'s presence row under `node` to say live.
fn live(fleet: &Fleet, node: &Node, sid: &str) {
    let subject = format!("/f/{FLEET}/pub/{}/{sid}/presence", node.node);
    until(&format!("{subject} to say live"), || {
        fleet
            .row(&subject)
            .filter(|b| b.contains("state=live"))
            .map(|_| ())
    });
}

/// The cross-node fleet: boot, measure, report.
pub(super) fn measure(spec: &Spec, clock: Clock) -> (Value, Vec<String>) {
    eprintln!("fabric bench: booting the cross-node fleet");
    let opts = opts_for(spec);
    let mut fleet = Fleet::boot_arm("fbench-x", opts.clone(), spec.retention);
    if let Some(ms) = spec.retention_interval_ms {
        fleet
            .broker()
            .set_retention_interval(Duration::from_millis(ms));
    }
    let flags: &[&str] = match spec.log {
        Log::Auto => &[],
        Log::Off => &["--log", "off"],
    };
    let env = [(wait_tally::SEAM, "1")];
    let a = fleet.node_with("fbench-xa", &[], flags, &env);
    let b = fleet.node_with("fbench-xb", &[], flags, &env);
    a.wait_ready(&fleet);
    b.wait_ready(&fleet);
    let (sa, _) = a.session();
    let (sb, _) = b.session();
    let sa2 = spawn_on(&a);
    for (node, sid) in [(&a, &sa), (&a, &sa2), (&b, &sb)] {
        live(&fleet, node, sid);
    }
    let served = served_line_of(fleet.broker(), spec);
    let ruler = Ruler::beside(&fleet.tmp, opts.durability, &fleet.sock);
    let mut cal_seq = 0;
    let cal_before = calibrate(&ruler, &a.node, clock, &mut cal_seq);
    let e = Ends {
        fleet: &fleet,
        a: &a,
        b: &b,
        sa: sa.clone(),
        sa2: sa2.clone(),
        sb: sb.clone(),
        clock,
        a_tally: a.state.join(wait_tally::FILE),
        a_pid: a.bridge_pid(),
        b_tally: b.state.join(wait_tally::FILE),
        b_pid: b.bridge_pid(),
    };
    let (by, heads) = sampled_on(fleet.broker(), clock, || {
        let mut ctl_a = a.ctl();
        let mut ctl_b = b.ctl();
        let mut since = Cursors {
            sa2: newest_id(&mut ctl_a, &sa2),
            sb: newest_id(&mut ctl_b, &sb),
        };
        let mut sent = e
            .settle(Form::Local, 0, 0, "the cross-node fleet's start")
            .tally
            .row(wait_tally::Row::LaneSent)
            .count;
        let mut prev = None;
        for i in 0..spec.warmup.max(1) {
            for form in Form::ALL {
                let text = format!("x-warm-{}-{i}", form.word());
                prev = Some(
                    one(
                        &e,
                        form,
                        (&mut ctl_a, &mut ctl_b),
                        &mut since,
                        &text,
                        &mut sent,
                    )
                    .1,
                );
            }
        }
        let mut prev = prev.expect("at least one warm-up round");
        let mut by = ByForm::default();
        eprintln!(
            "fabric bench: cross-node, {} posts of each form, one at a time",
            spec.samples
        );
        let mut i = 0;
        for (v, n) in blocks(spec.samples, Form::ALL.len()) {
            let form = Form::ALL[v];
            let mut last = None;
            for _ in 0..n {
                let text = format!("x-{}-{i}", form.word());
                i += 1;
                let (lat, mark) = one(
                    &e,
                    form,
                    (&mut ctl_a, &mut ctl_b),
                    &mut since,
                    &text,
                    &mut sent,
                );
                by.lat[v].push(lat);
                last = Some(mark);
            }
            let end = last.expect("a block posts at least once");
            by.lat_spent[v].add(&prev, &end, "the cross-node posts");
            prev = end;
        }
        eprintln!(
            "fabric bench: cross-node, a {}-message burst of each form, at most {BURST_WINDOW} in flight",
            spec.burst
        );
        for (k, (v, n)) in blocks(spec.burst, Form::ALL.len()).into_iter().enumerate() {
            let form = Form::ALL[v];
            let prefix = format!("xb-{}-{k}-", form.word());
            let (b, end) = burst(&e, form, n, &mut since, &prefix, &mut sent);
            by.bursts[v].push(b);
            by.burst_spent[v].add(&prev, &end, "the cross-node bursts");
            prev = end;
        }
        by
    });
    let cal_after = calibrate(&ruler, &a.node, clock, &mut cal_seq);
    let unit = yardstick(&cal_before, &cal_after);
    let (mut json, mut md) = report(&by, &heads, unit, spec, (&a.node, &b.node));
    if let Some(fields) = json.as_object_mut() {
        fields.insert("served".to_string(), json!(served.trim_end()));
        fields.insert(
            "yardstick".to_string(),
            yardstick_json(&ruler, &cal_before, &cal_after, unit),
        );
        fields.insert("head_changes_sampled".to_string(), json!(heads.acked.len()));
    }
    md.insert(1, served);
    md.push(String::new());
    md.extend(yardstick_rows(&ruler, &cal_before, &cal_after));
    (json, md)
}

/// One form's bursts, pooled: every message's post -> visible, and the
/// messages a second over the bursts' summed spans.
fn bursts_of(bursts: &[Burst]) -> (Vec<f64>, usize, f64) {
    let mut deliver = Vec::new();
    let (mut messages, mut span_ms) = (0, 0.0);
    for b in bursts {
        for (j, &t_ret) in b.rets.iter().enumerate() {
            if let Some(&(_, t)) = b.seen.get(j) {
                deliver.push(ms(t_ret, t));
            }
        }
        messages += b.seen.len();
        span_ms += b
            .seen
            .iter()
            .map(|&(_, t)| t)
            .max()
            .map_or(0.0, |t| ms(b.t0, t));
    }
    (deliver, messages, span_ms)
}

fn report(
    by: &ByForm,
    heads: &Heads,
    unit: Option<f64>,
    spec: &Spec,
    (na, nb): (&str, &str),
) -> (Value, Vec<String>) {
    let mut md = vec![
        format!("## Cross-node: a post between two aterms on one broker (nodes `{na}`, `{nb}`)\n"),
        format!(
            "Two headless aterms, each with its own bridge and the wait tally on, on one broker \
             in the bench process. Session A posts `kind=note` (no `--wait`) to {}, to {} and to \
             {}; the recipient's `await inbox` answers, it lists the row, and the next post waits \
             for the recipient's commit and for A's bridge to retire the post. {} of each form \
             one at a time, in blocks local, bare, at-node and back; then bursts, one sender, at \
             most {BURST_WINDOW} in flight, {} messages of each form over two bursts in the same \
             order. A's tally is the SENDER's work per post; on the local form it holds the \
             delivery and the commit too.\n",
            Form::Local.says(),
            Form::Bare.says(),
            Form::AtNode.says(),
            spec.samples,
            spec.burst,
        ),
        "### One at a time\n".to_string(),
        HEAD_U[0].to_string(),
        HEAD_U[1].to_string(),
    ];
    let mut lat_json = aterm_json::Map::new();
    for (v, form) in Form::ALL.iter().enumerate() {
        let lats = &by.lat[v];
        let mut call = Vec::new();
        let mut deliver = Vec::new();
        let mut to_acked = Vec::new();
        let mut acked_to_visible = Vec::new();
        for l in lats {
            let acked = heads.acked.landed(l.off);
            call.push(ms(l.t_call, l.t_ret));
            deliver.push(ms(l.t_ret, l.t_seen));
            to_acked.extend(ms_opt(Some(l.t_ret), acked));
            acked_to_visible.extend(ms_opt(acked, Some(l.t_seen)));
        }
        let w = form.word();
        md.push(row_u(
            &format!("{w}: post call (request -> `OK`)"),
            &call,
            unit,
        ));
        md.push(row_u(
            &format!("{w}: **post -> visible to the recipient**"),
            &deliver,
            unit,
        ));
        md.push(row_u(
            &format!("{w}: post -> acked on the broker"),
            &to_acked,
            unit,
        ));
        md.push(row_u(
            &format!("{w}: acked -> visible"),
            &acked_to_visible,
            unit,
        ));
        lat_json.insert(
            w.to_string(),
            json!({
                "posts": lats.len(),
                "post_call_ms": json_u(&call, unit),
                "post_to_visible_ms": json_u(&deliver, unit),
                "post_to_acked_ms": json_u(&to_acked, unit),
                "acked_to_visible_ms": json_u(&acked_to_visible, unit),
                "sender_steps": steps_json(&by.lat_spent[v], lats.len()),
            }),
        );
    }
    md.push(String::new());
    md.push("Where A's bridge spent a post, one at a time:\n".to_string());
    let cols: Vec<(&str, &Spent, usize)> = Form::ALL
        .iter()
        .enumerate()
        .map(|(v, f)| (f.word(), &by.lat_spent[v], by.lat[v].len()))
        .collect();
    md.extend(steps_rows(&cols));
    md.push(String::new());
    for (v, form) in Form::ALL.iter().enumerate() {
        md.push(flushes_per_post(
            &format!("Broker flushes, {}", form.word()),
            by.lat_spent[v].flushes,
            by.lat[v].len(),
        ));
        md.push(String::new());
    }
    md.extend([
        "### Bursts\n".to_string(),
        "| form | messages | every message visible (ms, summed over its bursts) | messages/s | \
         post -> visible p50 | p95 | p99 |"
            .to_string(),
        "|---|---:|---:|---:|---:|---:|---:|".to_string(),
    ]);
    let mut burst_json = aterm_json::Map::new();
    for (v, form) in Form::ALL.iter().enumerate() {
        let (deliver, messages, span_ms) = bursts_of(&by.bursts[v]);
        let rate = (span_ms > 0.0).then(|| messages as f64 / (span_ms / 1000.0));
        let s = stats::summarize(&deliver);
        let q = |f: fn(&stats::Summary) -> f64| {
            s.as_ref()
                .map_or("–".to_string(), |s| format!("{:.2}", f(s)))
        };
        md.push(format!(
            "| {} | {messages} | {span_ms:.1} | {} | {} | {} | {} |",
            form.word(),
            rate.map_or("–".to_string(), |r| format!("{r:.1}")),
            q(|s| s.p50),
            q(|s| s.p95),
            q(|s| s.p99),
        ));
        burst_json.insert(
            form.word().to_string(),
            json!({
                "bursts": by.bursts[v].len(),
                "messages": messages,
                "all_visible_ms_summed": stats::round3(span_ms),
                "visible_rate_per_s": rate.map(stats::round3),
                "post_to_visible_ms": json_u(&deliver, unit),
                "sender_steps": steps_json(&by.burst_spent[v], messages),
            }),
        );
    }
    md.push(String::new());
    md.push("Where A's bridge spent a burst message:\n".to_string());
    let cols: Vec<(&str, &Spent, usize)> = Form::ALL
        .iter()
        .enumerate()
        .map(|(v, f)| {
            let messages = by.bursts[v].iter().map(|b| b.seen.len()).sum();
            (f.word(), &by.burst_spent[v], messages)
        })
        .collect();
    md.extend(steps_rows(&cols));
    md.push(String::new());
    for (v, form) in Form::ALL.iter().enumerate() {
        let messages = by.bursts[v].iter().map(|b| b.seen.len()).sum();
        md.push(flushes_per_post(
            &format!("Broker flushes over the {} bursts", form.word()),
            by.burst_spent[v].flushes,
            messages,
        ));
        md.push(String::new());
    }
    let json = json!({
        "nodes": [na, nb],
        "one_at_a_time": Value::Object(lat_json),
        "bursts": Value::Object(burst_json),
    });
    (json, md)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_bursts_pool_their_messages_and_sum_their_spans() {
        use super::*;

        let bursts = [
            Burst {
                t0: 1_000,
                rets: vec![1_100, 1_200],
                seen: vec![(10, 1_500), (11, 3_000)],
            },
            Burst {
                t0: 10_000,
                rets: vec![10_100],
                seen: vec![(20, 11_000)],
            },
        ];
        let (deliver, messages, span) = bursts_of(&bursts);
        assert_eq!(deliver, vec![0.4, 1.8, 0.9]);
        assert_eq!(messages, 3);
        assert!((span - 3.0).abs() < 1e-9, "2 ms and 1 ms: {span}");
        assert_eq!(bursts_of(&[]), (Vec::new(), 0, 0.0));
    }
}
