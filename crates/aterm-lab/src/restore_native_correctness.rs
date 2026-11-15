// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Native-only first-window correctness. These receipts make no performance claim.

use super::*;

const HELP: &str = "restore-native --correctness-only --b APP --b-source SHA --b-sha256 SHA256 \
    --b-build-record JSONL --out NEW_DIR [--additional-window|--closed-reopen] [--keep]\n\
    Runs untouched and real-hardware-input native-only first-window restores with B.\n\
    Requires a genuine debug artifact; emits correctness.json and raw receipts, no timing CSV.\n\
    Holds shared window and quiet exclusion with a conservative 2 GiB memory budget;\n\
    CPU load and timing qualification are not prerequisites for this correctness check.\n\
    --additional-window instead restores eight editor tabs and an additional editor window\n\
    while 100 native keys target the terminal; uses 4 GiB functional admission and emits\n\
    no performance rows. Every key, editor, window, grid and saved layout must agree.\n\
    --closed-reopen checks ordinary closed tabs and closed-view graft/fallback routes,\n\
    with real native input during separate file and journal holds and canonical edit reuse.\n";
const READY: &str = "RESTORE_NATIVE_READY";
// Output establishes raw-mode readiness without admitting any input into the temporary shell.
const PROGRAM: &str = "stty raw -echo && printf 'RESTORE_NATIVE_READY' && exec /bin/cat";
const KEYS: u64 = 4;
const KEY_COUNT: usize = 4;
// Conservative resource budget for this two-document fixture, not a measured peak.
const WINDOW_MEMORY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn launch_correctness(world: &World, build: &Build, gated: bool) -> Result<Instance, String> {
    launch_with_program(
        world,
        build,
        gated,
        PROGRAM,
        RestoreWindow::Correctness {
            memory_bytes: WINDOW_MEMORY_BYTES,
        },
    )
}

fn read_shape(wire: &mut Wire, raw: &mut Raw, label: &str) -> Result<(Shape, String), String> {
    let text = common::counted(wire, "inspect app/v1 tabs")?;
    raw.note(label, &text);
    Ok((shape(&text)?, text))
}

fn terminal_view(text: &str, local: u64) -> Result<u64, String> {
    let mut found = None;
    for line in text
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("leaf "))
    {
        if fields(line).get("kind").map(String::as_str) == Some("terminal")
            && field(line, "session")? == local
            && found.replace(field(line, "view")?).is_some()
        {
            return Err("bootstrap occupies more than one view".into());
        }
    }
    found.ok_or("bootstrap view is absent".into())
}

/// Prove the exact terminal leaf owns focus, rather than only its tab/window.
fn focused_terminal_tab(text: &str, window: u64, local: u64, view: u64) -> Result<u64, String> {
    if terminal_view(text, local)? != view {
        return Err("typing terminal view identity changed".into());
    }
    let mut current_window = None;
    let mut current_tab = None;
    let mut focused_windows = 0;
    let mut focused_leaves = 0;
    let mut found = None;
    for line in text.lines().map(str::trim) {
        let words = fields(line);
        if line.starts_with("window ") {
            let focused = words.get("focused").map(String::as_str) == Some("true");
            focused_windows += usize::from(focused);
            current_window = Some((field(line, "id")?, focused, field(line, "active")?));
            current_tab = None;
        } else if line.starts_with("tab ") {
            current_tab = Some((
                field(line, "id")?,
                words.get("active").map(String::as_str) == Some("true"),
            ));
        } else if line.starts_with("leaf ")
            && words.get("focused").map(String::as_str) == Some("true")
        {
            let (wid, window_focused, active) = current_window.ok_or("leaf has no window")?;
            let (tab, tab_active) = current_tab.ok_or("leaf has no tab")?;
            if window_focused && tab_active && tab == active {
                focused_leaves += 1;
                if wid == window
                    && words.get("kind").map(String::as_str) == Some("terminal")
                    && field(line, "session")? == local
                    && field(line, "view")? == view
                {
                    found = Some(tab);
                }
            }
        }
    }
    if focused_windows != 1 || focused_leaves != 1 {
        return Err("typing focus is absent or ambiguous".into());
    }
    found.ok_or("original typing terminal is not the focused leaf".into())
}

fn native_rows(topology: &Shape, shift: usize) -> Result<BTreeMap<String, String>, String> {
    topology
        .rows
        .iter()
        .filter_map(|row| {
            let (key, rest) = row.strip_prefix("leaf ")?.split_once(' ')?;
            rest.starts_with("app ").then_some((key, rest))
        })
        .map(|(key, rest)| {
            let mut parts = key.splitn(3, ':');
            let window = parts.next().ok_or("native window missing")?;
            let tab: usize = parts
                .next()
                .ok_or("native tab missing")?
                .parse()
                .map_err(|_| "native tab invalid")?;
            let path = parts.next().ok_or("native path missing")?;
            Ok((format!("{window}:{}:{path}", tab + shift), rest.to_owned()))
        })
        .collect()
}

fn finished_shape(expected: &Shape, actual: &Shape, used: bool) -> Result<(), String> {
    let shift = usize::from(used);
    let mut counts = vec![(0, 1); shift];
    counts.extend(&expected.tab_counts);
    let mut kinds = vec!["terminal"; shift];
    kinds.extend(expected.tab_kinds.iter().map(String::as_str));
    if actual.window_tabs != [expected.tabs + shift]
        || actual.active_tabs != [1]
        || actual.tab_counts != counts
        || actual.tab_kinds.iter().map(String::as_str).ne(kinds)
        || actual.rows.first() != expected.rows.first()
        || native_rows(actual, 0)? != native_rows(expected, shift)?
    {
        return Err("native-only completion changed window/tab/leaf placement".into());
    }
    if !used {
        same_shape(expected, actual)?;
    }
    Ok(())
}

fn seed(
    world: &World,
    build: &Build,
    raw: &mut Raw,
) -> Result<(fixture::Composition, Shape), String> {
    common::digest(build, raw, "seed_before")?;
    let mut inst = launch_correctness(world, build, false)?;
    let result = (|| {
        inst.await_socket(instance::BOOT)
            .map_err(|f| f.msg.unwrap_or_else(|| "seed boot failed".into()))?;
        common::cpu(&inst, build, None, raw, "seed_identity")?;
        let mut wire = common::bounded_wire(&inst)?;
        let bootstrap = common::await_roster(&mut wire, 1)?.remove(0);
        common::await_text(&mut wire, &bootstrap.sid, READY, true)?;
        let mut uris = Vec::new();
        for n in 0..2 {
            let path = world.home.join(format!("native-only-{n}.txt"));
            std::fs::write(&path, format!("Native-only document {n}\n"))
                .map_err(|e| e.to_string())?;
            let uri = format!("file://{}", path.display());
            common::ask(&mut wire, &format!("open app editor {uri}"))?;
            uris.push(uri);
            wait(|| {
                let topology = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
                Ok((topology.tabs == n + 2 && topology.native == n + 1).then_some(()))
            })?;
        }
        common::ask(&mut wire, &format!("raise {}", bootstrap.sid))?;
        request_fixture_action("CloseTab", "close-tab", |command| {
            let reply = wire.request_line(command).map_err(|e| e.to_string());
            raw.note("seed_close_bootstrap", format!("{command}: {reply:?}"));
            reply
        })?;
        wait(|| {
            let topology = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
            Ok((topology.window_tabs == [2]
                && topology.native == 2
                && topology.terminal.is_empty())
            .then_some(()))
        })?;
        common::await_roster(&mut wire, 0)?;
        common::ask(&mut wire, "tab 1")?;
        let topology = get_shape(&mut wire, raw, "genuine_native_only_seed")?;
        let text = quit_layout(world, &inst, &mut wire, raw, "native_only_seed_layout")?;
        let saved = fixture::capture_native_only(&text, &uris)?;
        Ok((saved, topology))
    })();
    let (saved, expected) = common::finish(result, || inst.stop(), raw)?;

    // Qualify the real native-only capture with B without touching its temporary bootstrap.
    let mut inst = launch_correctness(world, build, false)?;
    let result = (|| {
        inst.await_socket(instance::BOOT)
            .map_err(|f| f.msg.unwrap_or_else(|| "normalization boot failed".into()))?;
        common::cpu(&inst, build, None, raw, "normalization_identity")?;
        let mut wire = common::bounded_wire(&inst)?;
        let normalized = wait(|| {
            let topology = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
            Ok(same_shape(&expected, &topology).is_ok().then_some(topology))
        })?;
        common::await_roster(&mut wire, 0)?;
        check_documents(
            &mut wire,
            raw,
            "normalized_documents",
            &normalized,
            &saved.native_uris()?,
        )?;
        let text = quit_layout(
            world,
            &inst,
            &mut wire,
            raw,
            "normalized_native_only_layout",
        )?;
        saved.check_round_trip(&text)?;
        common::digest(build, raw, "seed_end")?;
        Ok((saved, expected))
    })();
    common::finish(result, || inst.stop(), raw)
}

fn take(
    world: &World,
    build: &Build,
    used: bool,
    raw: &mut Raw,
) -> Result<aterm_json::Value, String> {
    let (saved, expected) = seed(world, build, raw)?;
    let uris = saved.native_uris()?;
    let wait_uri = uris.get("1:1:/").ok_or("first native URI missing")?;
    let gates = world.home.join("restore-measure");
    crate::util::create_private_dir_all(&gates).map_err(|e| e.to_string())?;
    private_write(
        &gates,
        "config.json",
        &aterm_json::json!({
            "start_file":"restore-start", "events_file":"restore-events.jsonl",
            "wait_uri":wait_uri, "release_file":"restore-release"
        })
        .to_string(),
    )?;
    common::digest(build, raw, "before_restore")?;
    let mut inst = launch_correctness(world, build, true)?;
    let result = (|| {
        inst.await_socket(instance::BOOT)
            .map_err(|f| f.msg.unwrap_or_else(|| "restore boot failed".into()))?;
        common::cpu(&inst, build, None, raw, "restore_identity")?;
        let pid = inst.aterm_pid().ok_or("restore PID missing")?;
        let events = gates.join("restore-events.jsonl");
        let mut wire = common::bounded_wire(&inst)?;
        wait(|| event(&events, "start_ready", pid))?;
        let typing = common::await_roster(&mut wire, 1)?.remove(0);
        common::await_text(&mut wire, &typing.sid, READY, true)?;
        let (initial, before) = read_shape(&mut wire, raw, "temporary_bootstrap")?;
        if initial.window_tabs != [1] || initial.tab_counts != [(0, 1)] {
            return Err("start gate did not retain exactly the temporary bootstrap".into());
        }
        let view = terminal_view(&before, typing.local)?;
        common::focus(&mut wire, &typing, None)?;
        let mut before_burst = None;
        if used {
            raw.note("before_restore_hwkeys", common::post(&mut wire, KEYS, 40)?);
            common::await_text(
                &mut wire,
                &typing.sid,
                &format!("{READY}{}", "a".repeat(KEY_COUNT)),
                true,
            )?;
            let metrics = common::ask(&mut wire, "metrics percentiles")?;
            raw.note("before_restore_metrics", &metrics);
            before_burst = Some(observation(&metrics)?.burst);
        }
        private_write(&gates, "restore-start", "start\n")?;
        let first = wait(|| event(&events, "wait_entered", pid))?;
        let mut during = None;
        if used {
            raw.note("held_reader_hwkeys", common::post(&mut wire, KEYS, 40)?);
            common::await_text(
                &mut wire,
                &typing.sid,
                &format!("{READY}{}", "a".repeat(KEY_COUNT * 2)),
                true,
            )?;
            let metrics = common::ask(&mut wire, "metrics percentiles")?;
            raw.note("held_reader_metrics", &metrics);
            let observed = observation(&metrics)?;
            if observed.burst == before_burst.unwrap_or(0)
                || observed.posted != KEYS
                || !release_absent(&gates.join("restore-release"))?
                || event(&events, "wait_released", pid)?.is_some()
            {
                return Err("hardware input was not completed while admission stayed held".into());
            }
            let posts = posted_times(&events, pid, &observed)?;
            during = Some((observed, posts));
        }
        private_write(&gates, "restore-release", "release\n")?;
        let last = wait(|| event(&events, "wait_released", pid))?;
        let observed = wait(|| {
            let observed = observation(&common::ask(&mut wire, "metrics")?)?;
            Ok(observed.finished.then_some(observed))
        })?;
        if !observed.enabled || observed.refused || !observed.gate_released {
            return Err("native-only restore measurement gate refused".into());
        }
        if let Some((_, posts)) = &during {
            if cohort(posts, first, last) != (KEY_COUNT, 0) {
                return Err(
                    "held hardware burst was not entirely inside file admission wait".into(),
                );
            }
        } else if observed.posted != 0 {
            return Err("untouched bootstrap received native input".into());
        }
        let (actual, after) = read_shape(&mut wire, raw, "completed_native_only_restore")?;
        finished_shape(&expected, &actual, used)?;
        if used {
            let rows = terminal_rows(&mut wire, &actual)?;
            let restored = rows.get("1:1:/").ok_or("preserved bootstrap missing")?;
            same_bootstrap(&typing, restored)?;
            if terminal_view(&after, typing.local)? != view {
                return Err("bootstrap view was replaced".into());
            }
            common::focus(&mut wire, &typing, None)?;
            common::await_text(
                &mut wire,
                &typing.sid,
                &format!("{READY}{}", "a".repeat(KEY_COUNT * 2)),
                true,
            )?;
            common::snapshot(&mut wire, &typing.sid, "preserved_bootstrap", raw)?;
        } else {
            common::await_roster(&mut wire, 0)?;
            if after.lines().any(|line| {
                fields(line)
                    .get("view")
                    .is_some_and(|s| s == &view.to_string())
            }) {
                return Err("untouched bootstrap view survived".into());
            }
        }
        let expected_uris = uris
            .iter()
            .map(|(key, uri)| {
                let key = if used {
                    let (_, rest) = key.split_once(':').expect("window-qualified key");
                    let (tab, path) = rest.split_once(':').expect("tab-qualified key");
                    format!(
                        "1:{}:{path}",
                        tab.parse::<usize>().expect("numeric tab") + 1
                    )
                } else {
                    key.clone()
                };
                (key, uri.clone())
            })
            .collect();
        check_documents(
            &mut wire,
            raw,
            "completed_documents",
            &actual,
            &expected_uris,
        )?;
        let layout = quit_layout(world, &inst, &mut wire, raw, "completed_layout")?;
        if used {
            saved.check_with_preserved_bootstrap(&layout, &typing.sid)?;
        } else {
            saved.check_round_trip(&layout)?;
        }
        common::digest(build, raw, "end")?;
        raw.note(
            "gate_events",
            std::fs::read_to_string(&events).map_err(|e| e.to_string())?,
        );
        Ok(aterm_json::json!({
            "case": if used { "hardware-used" } else { "untouched" },
            "passed":true, "bootstrap_sid":typing.sid, "bootstrap_local":typing.local,
            "bootstrap_view":view, "preserved":used, "native_tabs":2,
            "posted_before":if used { KEYS } else { 0 }, "posted_while_held":if used { KEYS } else { 0 },
            "layout_sha256":crate::measure::sha256(&layout)
        }))
    })();
    if result.is_err() {
        raw.note("restore_app_log", inst.log_tail(80));
    }
    let _ = private_write(&gates, "restore-start", "start\n");
    let _ = private_write(&gates, "restore-release", "release\n");
    common::finish(result, || inst.stop(), raw)
}

pub(super) fn run(args: &[String]) -> Res {
    if args.iter().any(|arg| arg == "--help") {
        println!("{HELP}");
        return Ok(0);
    }
    let mut args = args.to_vec();
    take_flag(&mut args, "--correctness-only");
    let additional_window = take_flag(&mut args, "--additional-window");
    let closed_reopen = take_flag(&mut args, "--closed-reopen");
    let build = Build::parse(&mut args, "b")?;
    let out = PathBuf::from(
        take_value(&mut args, "--out")?.ok_or_else(|| Fail::usage("--out is required"))?,
    );
    let keep = take_flag(&mut args, "--keep");
    if !args.is_empty() || !out.is_absolute() {
        return Err(Fail::usage(
            "use an absolute --out and only the B artifact options",
        ));
    }
    stop::install();
    std::fs::create_dir(&out).map_err(|e| Fail::exit(3, e.to_string()))?;
    if closed_reopen {
        return closed_reopen_case(build, out, keep);
    }
    if additional_window {
        return additional_window_case(build, out, keep);
    }
    let mut results = Vec::new();
    for used in [false, true] {
        let mut raw = Raw::default();
        raw.note("trusted_artifact", &build.artifact);
        raw.note("trusted_build_record", build.record.display());
        raw.note("trusted_build_record_sha256", &build.record_digest);
        let config = format!(
            "{}{}",
            instance::config_with(&[]),
            crate::linux_proof::DECK_OFF
        );
        let mut world = World::new(&config).map_err(|e| Fail::exit(3, e))?;
        if keep {
            world.keep();
        }
        raw.note("world", world.dir.display());
        let result = take(&world, &build, used, &mut raw);
        if result.is_err() {
            world.keep();
        }
        raw.note("result", format!("{result:?}"));
        let path = out.join(if used {
            "hardware-used.txt"
        } else {
            "untouched.txt"
        });
        std::fs::write(&path, raw.as_text()).map_err(|e| Fail::exit(3, e.to_string()))?;
        results.push(result.map_err(|e| {
            Fail::exit(
                3,
                format!("native-only correctness refused; {}: {e}", path.display()),
            )
        })?);
    }
    let receipt = aterm_json::json!({
        "kind":"native-only-restore-correctness", "source":build.source, "sha256":build.digest,
        "admission":"correctness", "timing_qualified":false,
        "memory_budget_bytes":WINDOW_MEMORY_BYTES, "cases":results
    });
    let text = format!("{receipt}\n");
    std::fs::write(out.join("correctness.json"), &text)
        .map_err(|e| Fail::exit(3, e.to_string()))?;
    print!("{text}");
    Ok(0)
}

fn additional_window_case(build: Build, out: PathBuf, keep: bool) -> Res {
    // A conservative reservation for this fixed nine-document, two-window workload.
    // Its resource class qualifies correctness, never a timing or performance claim.
    const MEMORY: u64 = 4 * 1024 * 1024 * 1024;
    let case = Case::EditorFirstNativeWindow;
    let opts = Options {
        window: RestoreWindow::Correctness {
            memory_bytes: MEMORY,
        },
        builds: [build.clone(), build.clone()],
        out: out.clone(),
        cases: vec![case],
        modes: vec![Mode::Natural],
        tabs: 8,
        leaves: 16,
        keys: 100,
        key_ms: 20,
        wait_ms: 1200,
        quads: 1,
        keep,
    };
    let mut raw = Raw::default();
    raw.note("trusted_artifact", &build.artifact);
    raw.note("trusted_build_record", build.record.display());
    raw.note("trusted_build_record_sha256", &build.record_digest);
    raw.note("admission", "correctness; no timing qualification");
    let config = format!(
        "{}{}",
        instance::config_with(&[]),
        crate::linux_proof::DECK_OFF
    );
    let mut world = World::new(&config).map_err(|e| Fail::exit(3, e))?;
    if keep {
        world.keep();
    }
    raw.note("world", world.dir.display());
    let result = super::seed(&world, &build, case, &opts, &mut raw)
        .and_then(|seed| measured(&world, &build, &seed, Mode::Natural, true, &opts, &mut raw));
    if result.is_err() {
        world.keep();
    }
    raw.note("result", format!("{result:?}"));
    let path = out.join("additional-window.txt");
    std::fs::write(&path, raw.as_text()).map_err(|e| Fail::exit(3, e.to_string()))?;
    let taken = result.map_err(|e| {
        Fail::exit(
            3,
            format!(
                "additional-window correctness refused; {}: {e}",
                path.display()
            ),
        )
    })?;
    let receipt = aterm_json::json!({
        "kind":"additional-window-restore-correctness", "source":build.source,
        "sha256":build.digest, "admission":"correctness", "timing_qualified":false,
        "memory_budget_bytes":MEMORY, "passed":true, "keys":opts.keys,
        "windows":taken.windows, "documents":9, "posts_during_restore":taken.posts_during_restore,
        "bootstrap_position":taken.bootstrap_key, "layout_sha256":taken.layout
    });
    let text = format!("{receipt}\n");
    std::fs::write(out.join("correctness.json"), &text)
        .map_err(|e| Fail::exit(3, e.to_string()))?;
    print!("{text}");
    Ok(0)
}

#[cfg(test)]
#[path = "restore_native_correctness_tests.rs"]
mod tests;

fn closed_document_text(wire: &mut Wire, document: u64) -> Result<(String, String), String> {
    let (head, bytes) = wire
        .request_bytes(&format!("doc read {document}"))
        .map_err(|error| error.to_string())?;
    if !head.starts_with("OK") {
        return Err(head);
    }
    Ok((
        head,
        String::from_utf8(bytes).map_err(|error| error.to_string())?,
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ClosedRoute {
    Tab,
    ViewGraft,
    ViewFallback,
}
impl ClosedRoute {
    fn name(self) -> &'static str {
        match self {
            Self::Tab => "tab",
            Self::ViewGraft => "view-graft",
            Self::ViewFallback => "view-fallback",
        }
    }
    fn action(self) -> &'static str {
        if self == Self::Tab {
            "ReopenClosedTab"
        } else {
            "ReopenClosedView"
        }
    }
    fn native_key(self) -> &'static str {
        if self == Self::ViewGraft {
            "1:1:/1"
        } else {
            "1:2:/"
        }
    }
}

fn closed_fixture_custody(wire: &mut Wire, raw: &mut Raw, held: bool) -> Result<(), String> {
    let metrics = common::ask(wire, "metrics")?;
    raw.note(
        if held {
            "held_candidate_custody"
        } else {
            "published_candidate_custody"
        },
        &metrics,
    );
    for (name, expected) in [
        ("fixture_candidate_retained", u64::from(held)),
        ("fixture_closed_operations", u64::from(held)),
        ("fixture_unpublished_attachments", 0),
    ] {
        if field(&metrics, &format!("restore_measurement_{name}"))? != expected {
            return Err(format!("closed fixture {name} disagrees with held={held}"));
        }
    }
    Ok(())
}

fn judge_closed_shape(
    route: ClosedRoute,
    before: &Shape,
    after: &Shape,
    typing: u64,
) -> Result<(), String> {
    let expected_counts = match route {
        ClosedRoute::Tab => vec![(0, 1), (1, 0)],
        ClosedRoute::ViewGraft => vec![(1, 1)],
        ClosedRoute::ViewFallback => vec![(0, 2), (1, 0)],
    };
    let expected_terminal = if route == ClosedRoute::ViewGraft {
        BTreeMap::from([("1:1:/0".to_owned(), typing)])
    } else {
        before.terminal.clone()
    };
    if after.window_ids != before.window_ids
        || after.tab_counts != expected_counts
        || after
            .native_views
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != [route.native_key()]
        || after.terminal != expected_terminal
        || after.active_tabs
            != [if route == ClosedRoute::ViewGraft {
                1
            } else {
                2
            }]
        || !after.rows.iter().any(|row| {
            row.starts_with(&format!("leaf {} app ", route.native_key()))
                && row.split_whitespace().nth(4) == Some("true")
        })
    {
        return Err("closed reopen changed its destination, survivor or native placement".into());
    }
    if route == ClosedRoute::ViewGraft
        && !after
            .rows
            .iter()
            .any(|row| row.starts_with("split 1:1 / horizontal 0.3700 "))
    {
        return Err("closed view graft changed saved split order or ratio".into());
    }
    if route == ClosedRoute::ViewFallback {
        let splits = |shape: &Shape| {
            shape
                .rows
                .iter()
                .filter(|row| row.starts_with("split 1:1 "))
                .cloned()
                .collect::<Vec<_>>()
        };
        if splits(before) != splits(after) {
            return Err("closed view fallback replaced current branches".into());
        }
    }
    Ok(())
}

fn closed_reopen_case(build: Build, out: PathBuf, keep: bool) -> Res {
    let mut cases = Vec::new();
    for route in [
        ClosedRoute::Tab,
        ClosedRoute::ViewGraft,
        ClosedRoute::ViewFallback,
    ] {
        cases.push(closed_reopen_take(&build, &out, keep, route)?);
    }
    let receipt = aterm_json::json!({"kind":"closed-reopen-native-correctness", "passed":true,
        "source":build.source, "sha256":build.digest, "timing_qualified":false,
        "admission":"correctness", "memory_budget_bytes":WINDOW_MEMORY_BYTES, "cases":cases});
    std::fs::write(out.join("correctness.json"), format!("{receipt}\n"))
        .map_err(|e| Fail::exit(3, e.to_string()))?;
    println!("{receipt}");
    Ok(0)
}

/// Functional native input proof at real closed-history read and journal boundaries.
fn closed_reopen_take(
    build: &Build,
    out: &Path,
    keep: bool,
    route: ClosedRoute,
) -> Result<aterm_json::Value, Fail> {
    let mut raw = Raw::default();
    raw.note("trusted_artifact", &build.artifact);
    raw.note("trusted_build_record", build.record.display());
    raw.note("trusted_build_record_sha256", &build.record_digest);
    let config = format!(
        "{}{}",
        instance::config_with(&[]),
        crate::linux_proof::DECK_OFF
    );
    let mut world = World::new(&config).map_err(|e| Fail::exit(3, e))?;
    if keep {
        world.keep();
    }
    raw.note("world", world.dir.display());
    let gates = world.home.join("restore-measure");
    crate::util::create_private_dir_all(&gates).map_err(|e| Fail::exit(3, e.to_string()))?;
    let path = world.home.join("closed-reopen.txt");
    std::fs::write(&path, "Closed history native correctness\n")
        .map_err(|e| Fail::exit(3, e.to_string()))?;
    let uri = format!("file://{}", path.display());
    private_write(
        &gates,
        "config.json",
        &aterm_json::json!({
            "start_file":"restore-start", "events_file":"restore-events.jsonl",
            "wait_uri":uri, "release_file":"restore-release",
            "closed_reopen":true, "journal_release_file":"journal-release",
            "seed_closed_reopen":true, "seed_closed_view":route != ClosedRoute::Tab
        })
        .to_string(),
    )
    .map_err(|e| Fail::exit(3, e))?;
    let mut inst = launch_correctness(&world, build, true).map_err(|e| Fail::exit(3, e))?;
    let result = (|| -> Result<aterm_json::Value, String> {
        inst.await_socket(instance::BOOT)
            .map_err(|f| f.msg.unwrap_or_else(|| "closed reopen boot failed".into()))?;
        common::cpu(&inst, build, None, &mut raw, "reopen_identity")?;
        common::digest(build, &mut raw, "before_reopen")?;
        let pid = inst.aterm_pid().ok_or("closed reopen PID missing")?;
        let events = gates.join("restore-events.jsonl");
        let mut wire = common::bounded_wire(&inst)?;
        raw.note("phase", "await start_ready");
        wait(|| event(&events, "start_ready", pid))?;
        raw.note("phase", "start_ready observed");
        private_write(&gates, "restore-start", "start\n")?;
        raw.note("phase", "await start_released");
        wait(|| event(&events, "start_released", pid))?;
        raw.note("phase", "start_released observed");
        raw.note("phase", "await bootstrap roster");
        let typing = common::await_roster(&mut wire, 1)?.remove(0);
        raw.note("phase", "bootstrap roster observed");
        raw.note("phase", "await bootstrap text");
        common::await_text(&mut wire, &typing.sid, READY, true)?;
        raw.note("phase", "bootstrap text observed");
        raw.note("phase", "await reopen_fixture_seeded");
        wait(|| event(&events, "reopen_fixture_seeded", pid))?;
        raw.note("phase", "reopen_fixture_seeded observed");
        let (initial, _) = read_shape(&mut wire, &mut raw, "identity_free_history_fixture")?;
        if initial.tabs != 1 || initial.native != 0 {
            return Err("closed history fixture published a document before reopen".into());
        }
        let documents = common::counted(&mut wire, "doc ls")?;
        raw.note("fixture_documents_before_reopen", &documents);
        if documents.lines().skip(1).any(|line| !line.is_empty()) {
            return Err("identity-free fixture unexpectedly owns a canonical document".into());
        }
        common::focus(&mut wire, &typing, None)?;
        let original_topology = common::counted(&mut wire, "inspect app/v1 tabs")?;
        let original_terminal_view = terminal_view(&original_topology, typing.local)?;
        let typing_window = typing
            .window
            .parse()
            .map_err(|_| "typing window is invalid")?;
        let original_terminal_tab = focused_terminal_tab(
            &original_topology,
            typing_window,
            typing.local,
            original_terminal_view,
        )?;
        // The protocol response itself proves this caller returned with the read still held.
        raw.note(
            "reopen_response",
            common::ask(&mut wire, &format!("invoke {}", route.action()))?,
        );
        raw.note("phase", "await read wait_entered");
        let read_first = wait(|| event(&events, "wait_entered", pid))?;
        raw.note("phase", "read wait_entered observed");
        raw.note("phase", "await reopen_canonical_absent");
        wait(|| event(&events, "reopen_canonical_absent", pid))?;
        raw.note("phase", "reopen_canonical_absent observed");
        raw.note(
            "coalesced_response",
            common::ask(&mut wire, &format!("invoke {}", route.action()))?,
        );
        if route == ClosedRoute::ViewFallback {
            // A genuine host edit while read is held invalidates only the graft,
            // so the same prepared leaf must land in a new singleton tab.
            common::ask(&mut wire, "spawn split=v")?;
            common::await_roster(&mut wire, 2)?;
            // Splitting selects the new pane. Restore the input target once as setup;
            // the per-burst checks below only observe and never rescue focus.
            raw.note(
                "fallback_setup_raise",
                common::raise_for_setup(&mut wire, &typing)?,
            );
        }
        let held_baseline = get_shape(&mut wire, &mut raw, "held_survivor_topology")?;
        let mut intervals = Vec::new();
        for (stage, release, entered, released, first, keys) in [
            (
                "read",
                "restore-release",
                "wait_entered",
                "wait_released",
                read_first,
                KEY_COUNT,
            ),
            (
                "journal",
                "journal-release",
                "journal_wait_entered",
                "journal_wait_released",
                0,
                KEY_COUNT * 2,
            ),
        ] {
            let first = if first == 0 {
                raw.note("phase", format!("await {stage} {entered}"));
                let observed = wait(|| event(&events, entered, pid))?;
                raw.note("phase", format!("{stage} {entered} observed"));
                observed
            } else {
                first
            };
            let (held, held_text) =
                read_shape(&mut wire, &mut raw, &format!("{stage}_held_topology"))?;
            if focused_terminal_tab(
                &held_text,
                typing_window,
                typing.local,
                original_terminal_view,
            )? != original_terminal_tab
            {
                return Err("typing terminal moved to another tab while held".into());
            }
            same_shape(&held_baseline, &held)?;
            if held.native != 0 || held.terminal != held_baseline.terminal {
                return Err(format!(
                    "{stage} published a prefix or replaced the survivor"
                ));
            }
            closed_fixture_custody(&mut wire, &mut raw, true)?;
            common::focus(&mut wire, &typing, None)?;
            raw.note(
                &format!("{stage}_hardware_keys"),
                common::post(&mut wire, KEYS, 40)?,
            );
            common::await_text(
                &mut wire,
                &typing.sid,
                &format!("{READY}{}", "a".repeat(keys)),
                true,
            )?;
            let metrics = common::ask(&mut wire, "metrics percentiles")?;
            raw.note(&format!("{stage}_metrics"), &metrics);
            let observed = observation(&metrics)?;
            if observed.posted != KEYS
                || !release_absent(&gates.join(release))?
                || event(&events, released, pid)?.is_some()
            {
                return Err(format!(
                    "native input did not complete inside the {stage} hold"
                ));
            }
            let posts = posted_times(&events, pid, &observed)?;
            private_write(&gates, release, "release\n")?;
            raw.note("phase", format!("await {stage} {released}"));
            let last = wait(|| event(&events, released, pid))?;
            raw.note("phase", format!("{stage} {released} observed"));
            if cohort(&posts, first, last) != (KEY_COUNT, 0) {
                return Err(format!("hardware posts escaped the actual {stage} hold"));
            }
            intervals.push(
                aterm_json::json!({"stage":stage,"first_ns":first,"last_ns":last,"keys":KEYS}),
            );
        }
        let actual = wait(|| {
            let topology = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
            Ok(
                judge_closed_shape(route, &held_baseline, &topology, typing.local)
                    .is_ok()
                    .then_some(topology),
            )
        })?;
        closed_fixture_custody(&mut wire, &mut raw, false)?;
        let (_, restored_text) = read_shape(&mut wire, &mut raw, "completed_closed_topology")?;
        if terminal_view(&restored_text, typing.local)? != original_terminal_view {
            return Err("closed view replaced its live terminal view identity".into());
        }
        check_documents(
            &mut wire,
            &mut raw,
            "reopened_document",
            &actual,
            &BTreeMap::from([(route.native_key().to_owned(), uri.clone())]),
        )?;
        // A separate ordinary close/open/edit/reopen genuinely reuses the canonical head.
        let docs = common::counted(&mut wire, "doc ls")?;
        raw.note("canonical_before_close", &docs);
        let document = docs
            .lines()
            .find_map(|line| {
                fields(line)
                    .get("doc")
                    .and_then(|id| id.parse::<u64>().ok())
            })
            .ok_or("canonical document identity missing")?;
        request_fixture_action(
            "CloseTab",
            if route == ClosedRoute::ViewGraft {
                "close-pane"
            } else {
                "close-tab"
            },
            |command| wire.request_line(command).map_err(|e| e.to_string()),
        )?;
        wait(|| {
            let topology = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
            Ok((topology.tabs == 1).then_some(()))
        })?;
        common::ask(&mut wire, &format!("open app editor {uri}"))?;
        wait(|| {
            let topology = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
            Ok((topology.tabs == 2 && topology.native == 1).then_some(()))
        })?;
        raw.note(
            "canonical_editor_hardware_keys",
            common::post(&mut wire, KEYS, 40)?,
        );
        let edited = wait(|| {
            let (head, text) = closed_document_text(&mut wire, document)?;
            raw.note("canonical_edit_read", format!("{head}\n{text}"));
            Ok(text
                .contains(&format!("{}Closed history", "a".repeat(KEY_COUNT)))
                .then_some(text))
        })?;
        raw.note(
            "canonical_reopen_response",
            common::ask(
                &mut wire,
                if route == ClosedRoute::ViewGraft {
                    "invoke ReopenClosedView"
                } else {
                    "invoke ReopenClosedTab"
                },
            )?,
        );
        wait(|| event(&events, "reopen_canonical_reused", pid))?;
        let reused = wait(|| {
            let topology = shape(&common::counted(&mut wire, "inspect app/v1 tabs")?)?;
            Ok((topology.tabs
                == (if route == ClosedRoute::ViewGraft {
                    2
                } else {
                    3
                })
                && topology.native == 2)
                .then_some(topology))
        })?;
        let shared = common::counted(&mut wire, "doc ls")?;
        raw.note("shared_canonical_document", &shared);
        let expected = if route == ClosedRoute::ViewGraft {
            BTreeMap::from([
                ("1:1:/1".to_owned(), uri.clone()),
                ("1:2:/".to_owned(), uri.clone()),
            ])
        } else {
            BTreeMap::from([
                ("1:2:/".to_owned(), uri.clone()),
                ("1:3:/".to_owned(), uri.clone()),
            ])
        };
        if document_positions_with_dirty(&reused, &shared, true)? != expected
            || shared
                .lines()
                .skip(1)
                .any(|line| field(line, "doc") != Ok(document))
        {
            return Err("reopen did not retain the same dirty canonical document".into());
        }
        let (head, after) = closed_document_text(&mut wire, document)?;
        raw.note("shared_canonical_after", format!("{head}\n{after}"));
        if after != edited {
            return Err("closed reopen changed the newer canonical edits".into());
        }
        common::await_roster(
            &mut wire,
            if route == ClosedRoute::ViewFallback {
                2
            } else {
                1
            },
        )?;
        common::snapshot(&mut wire, &typing.sid, "terminal_after_reopen", &mut raw)?;
        common::digest(build, &mut raw, "after_reopen")?;
        raw.note(
            "gate_events",
            std::fs::read_to_string(events).map_err(|e| e.to_string())?,
        );
        Ok(aterm_json::json!({
            "kind":"closed-reopen-native-correctness", "passed":true, "route":route.name(),
            "raw_file":format!("closed-reopen-{}.txt", route.name()),
            "source":build.source, "sha256":build.digest, "timing_qualified":false,
            "admission":"correctness", "memory_budget_bytes":WINDOW_MEMORY_BYTES,
            "routing":"ordinary-absent-canonical", "fixture":"identity-free-history-only",
            "canonical_reuse_with_newer_edits":true,
            "canonical_reuse_action":if route == ClosedRoute::ViewGraft { "ReopenClosedView" } else { "ReopenClosedTab" },
            "intervals":intervals
        }))
    })();
    if result.is_err() {
        world.keep();
        raw.note("reopen_app_log", inst.log_tail(80));
    }
    for name in ["restore-start", "restore-release", "journal-release"] {
        let _ = private_write(
            &gates,
            name,
            if name == "restore-start" {
                "start\n"
            } else {
                "release\n"
            },
        );
    }
    let result = common::finish(result, || inst.stop(), &mut raw);
    raw.note("result", format!("{result:?}"));
    std::fs::write(
        out.join(format!("closed-reopen-{}.txt", route.name())),
        raw.as_text(),
    )
    .map_err(|e| Fail::exit(3, e.to_string()))?;
    let receipt = result.map_err(|e| Fail::exit(3, e))?;
    Ok(receipt)
}
