// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Three real Settings regressions under explicit, non-powering Windows custody.
//! Application control input is labelled separately from USB keyboard/tablet input.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::system_state_measure::{self as measure, Layout};
use crate::windows::{self, HeldVm, Up};
use crate::windows_desk::{self as desk, ATERM_CLASS};
use crate::windows_guest::{self as guest, Arch};
use crate::windows_window::{self as window, Session};
use crate::{Fail, Res};

struct Ask {
    source: String,
    gui: String,
    ctl: String,
    cpu: bool,
    out: PathBuf,
}

fn parse(args: &[String]) -> Result<Ask, Fail> {
    let usage = || {
        Fail::usage(
            "windows attach <custody> settings --source SHA40 --gui-sha SHA256 --ctl-sha SHA256 --backend cpu|warp --out ABSOLUTE-NEW-DIRECTORY",
        )
    };
    let [s, source, g, gui, c, ctl, b, backend, o, out] = args else {
        return Err(usage());
    };
    let hex = |v: &str, n| v.len() == n && v.bytes().all(|b| b.is_ascii_hexdigit());
    if s != "--source"
        || g != "--gui-sha"
        || c != "--ctl-sha"
        || b != "--backend"
        || o != "--out"
        || !hex(source, 40)
        || !hex(gui, 64)
        || !hex(ctl, 64)
        || !matches!(backend.as_str(), "cpu" | "warp")
        || !Path::new(out).is_absolute()
    {
        return Err(usage());
    }
    Ok(Ask {
        source: source.to_ascii_lowercase(),
        gui: gui.to_ascii_lowercase(),
        ctl: ctl.to_ascii_lowercase(),
        cpu: backend == "cpu",
        out: out.into(),
    })
}

pub(crate) fn validate_args(args: &[String]) -> Result<(), Fail> {
    parse(args).map(|_| ())
}

fn retain(dir: &Path, name: &str, text: &str) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let path = dir.join(name);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|()| aterm_durable::durable(&file))
        .and_then(|()| aterm_durable::Flush::Durable.dir(dir))
        .map_err(|e| e.to_string())
}

fn hash_guest(up: &Up, file: &str) -> Result<String, String> {
    let out = up.ssh.ps(&format!("$ErrorActionPreference='Stop'; (Get-FileHash -Algorithm SHA256 -LiteralPath {}).Hash.ToLowerInvariant()", guest::ps_quote(file)));
    let value = out.text.trim();
    if !out.ok() || value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "cannot identify guest executable {file}: {}",
            out.tail(3)
        ));
    }
    Ok(value.to_owned())
}

struct Scenario<'a> {
    up: &'a Up,
    held: &'a HeldVm,
    session: &'a Session<'a>,
    out: &'a Path,
    native_release_unconfirmed: Cell<bool>,
}

impl Scenario<'_> {
    fn hit(&self, x: i32, y: i32) -> Result<String, String> {
        self.held
            .check(&self.up.layout)
            .map_err(|e| format!("{e:?}"))?;
        window::desk(
            self.up,
            &[
                "hit",
                &self.session.pid.to_string(),
                ATERM_CLASS,
                &x.to_string(),
                &y.to_string(),
            ],
        )
    }

    fn whole_named_at(&self, layout: &Layout, key: &str, x: i32, y: i32) -> Result<bool, String> {
        let placed = window::desk(
            self.up,
            &["place", &self.session.pid.to_string(), ATERM_CLASS],
        )?;
        let placed = desk::parse_place(&placed).ok_or("client origin unavailable")?;
        let point = (
            f64::from(x - placed.at.0) / layout.scale,
            f64::from(y - placed.at.1) / layout.scale,
        );
        Ok(Scenario::whole(layout, key)
            && layout
                .nodes
                .iter()
                .any(|n| n.key == key && n.rect.contains(point.0, point.1))
            && key
                .strip_prefix("settings/control/")
                .is_some_and(|setting| named_whole(layout, setting)))
    }
    fn only_window(&self) -> Result<desk::Window, String> {
        let raw = window::desk(self.up, &["windows", &self.session.pid.to_string()])?;
        let mut owned = desk::parse_windows(&raw)
            .into_iter()
            .filter(|w| w.class == ATERM_CLASS);
        let one = owned.next().ok_or("no owned visible app window")?;
        if owned.next().is_some() {
            return Err("multiple owned app windows; target is ambiguous".into());
        }
        Ok(one)
    }
    fn ctl(&self, args: &[&str]) -> Result<String, String> {
        self.held
            .check(&self.up.layout)
            .map_err(|e| format!("{e:?}"))?;
        self.session.ctl(args)
    }

    fn view(&self) -> Result<String, String> {
        settings_view(&self.ctl(&["inspect", "app/v1", "tabs"])?)
            .ok_or_else(|| "no actual Settings view".into())
    }

    fn layout(&self) -> Result<(String, Layout), String> {
        let view = self.view()?;
        let raw = self.ctl(&["inspect", "app/v1", "view", &view, "audit"])?;
        let layout = measure::parse_layout(&raw)?;
        if !layout.scale.is_finite() || layout.scale <= 0.0 {
            return Err("unknown actual viewport scale".into());
        }
        Ok((raw, layout))
    }

    fn open(&self, route: &str) -> Result<(), String> {
        self.ctl(&["open", "app", "settings", route])?;
        Ok(())
    }

    // Change only the owned app client size. Refuse an OS-clamped/unrepresentable
    // viewport rather than silently replacing the regression's content dimensions.
    fn viewport(&self, width: f64, height: f64) -> Result<(), String> {
        for _ in 0..4 {
            let (_, layout) = self.layout()?;
            if (layout.viewport.w - width).abs() <= 0.5 && (layout.viewport.h - height).abs() <= 0.5
            {
                return Ok(());
            }
            let w = self.only_window()?;
            let cw = (f64::from(w.client.0) + (width - layout.viewport.w) * layout.scale).round();
            let ch = (f64::from(w.client.1) + (height - layout.viewport.h) * layout.scale).round();
            if !(1.0..=8192.0).contains(&cw) || !(1.0..=8192.0).contains(&ch) {
                return Err("requested native viewport is unrepresentable".into());
            }
            self.ctl(&["resize", "px", &cw.to_string(), &ch.to_string()])?;
            self.settled()?;
        }
        Err(format!(
            "owned window cannot realize native content viewport {width}x{height}; no product verdict"
        ))
    }

    fn settled(&self) -> Result<(), String> {
        let end = Instant::now() + Duration::from_secs(60);
        let mut before = None;
        loop {
            let (_, now) = self.layout()?;
            if before.as_ref() == Some(&now) {
                return Ok(());
            }
            before = Some(now);
            if Instant::now() >= end {
                return Err("native geometry did not settle within hang detector".into());
            }
        }
    }

    fn capture(&self, label: &str, transport: &str) -> Result<Layout, String> {
        self.settled()?;
        let owned = self.only_window()?;
        let (audit, layout) = self.layout()?;
        let view = self.view()?;
        let mut text = format!(
            "input_transport={transport}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n",
            self.ctl(&["text-scale"])?,
            self.ctl(&["dims"])?,
            self.ctl(&["status"])
                .unwrap_or_else(|e| format!("status=unknown detail={e}")),
            self.ctl(&["metrics"])
                .unwrap_or_else(|e| format!("metrics=unknown detail={e}")),
            self.ctl(&["inspect", "app/v1", "tabs"])?,
            self.ctl(&["inspect", "app/v1", "view", &view, "tree"])?,
            audit
        );
        let remote = format!("{}\\{label}.ppm", self.session.dir);
        self.held
            .check(&self.up.layout)
            .map_err(|e| format!("{e:?}"))?;
        let readback = window::desk(
            self.up,
            &[
                "readback",
                &self.session.pid.to_string(),
                ATERM_CLASS,
                &remote,
            ],
        )?;
        desk::parse_place(&readback)
            .ok_or_else(|| format!("no Win32 client readback: {readback}"))?;
        text.push_str(&readback);
        text.push_str(&format!(
            "\nowned_window={owned:?}\nclient_readback_is_not_proof_of_offscreen_glass\n{}",
            window::desk(self.up, &["monitors"])?
        ));
        retain(self.out, &format!("{label}.txt"), &text)?;
        let local = self.out.join(format!("{label}.ppm"));
        let copied = self.up.ssh.get(&remote.replace('\\', "/"), &local);
        if !copied.ok() {
            return Err(format!("client pixels not retained: {}", copied.tail(3)));
        }
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&local, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| e.to_string())?;
        // Guest evidence is removed only after the host copy and its directory
        // entry are durable, including captures copied by the external transport.
        let (file, _) = aterm_uds::private_file::open_private(self.out, &format!("{label}.ppm"))
            .map_err(|e| e.to_string())?;
        aterm_durable::durable(&file)
            .and_then(|()| aterm_durable::Flush::Durable.dir(self.out))
            .map_err(|e| e.to_string())?;
        Ok(layout)
    }

    fn whole(layout: &Layout, key: &str) -> bool {
        layout.nodes.iter().any(|n| {
            n.key == key
                && n.visible
                && !n.clipped
                && !n.faded()
                && n.rect.w > 0.0
                && n.rect.h > 0.0
                && n.rect.intersect(n.clip) == n.rect
        })
    }

    fn scroll_to(&self, key: &str) -> Result<(), String> {
        // Each input is a distinct manual scroll, not a retry of a failed check.
        for _ in 0..32 {
            let (_, layout) = self.layout()?;
            if Self::whole(&layout, key) {
                return Ok(());
            }
            let xy = format!(
                "xy={},{}",
                layout.viewport.x + layout.viewport.w / 2.0,
                layout.viewport.y + layout.viewport.h / 2.0
            );
            self.ctl(&["pointer", "wheel", "dy=120", "unit=pt", &xy])?;
            self.settled()?;
        }
        Err(format!("{key} not whole after bounded manual scroll walk"))
    }
}

pub(crate) fn run(up: &Up, held: &HeldVm, args: &[String]) -> Res {
    let ask = parse(args)?;
    let result = run_inner(up, held, &ask);
    if let Err(reason) = &result {
        let _ = retain(&ask.out, "REFUSED.txt", reason);
    }
    result.map(|()| 0).map_err(Fail::die)
}

fn run_inner(up: &Up, held: &HeldVm, ask: &Ask) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&ask.out)
        .map_err(|e| format!("new private evidence directory: {e}"))?;
    let bin = crate::windows_smoke::bin_dir(Arch::Aarch64);
    let gui = hash_guest(up, &format!("{bin}\\aterm-gui.exe"))?;
    let ctl = hash_guest(up, &format!("{bin}\\aterm-ctl.exe"))?;
    if gui != ask.gui || ctl != ask.ctl {
        return Err("frozen guest executable hash mismatch; no window launched".into());
    }
    let driver = std::env::current_exe().map_err(|e| e.to_string())?;
    retain(
        &ask.out,
        "identity.txt",
        &format!(
            "declared_app_build_source={}\nsource_binding=external-retained-build-evidence\ngui_path={bin}\\aterm-gui.exe\ngui_sha256={gui}\nctl_sha256={ctl}\ndriver_path={}\ndriver_sha256={}\nbackend_requested={}\n",
            ask.source,
            driver.display(),
            windows::sha256_file(&driver)?,
            if ask.cpu { "cpu" } else { "warp" }
        ),
    )?;
    held.check(&up.layout).map_err(|e| format!("{e:?}"))?;
    window::put_helper(up)?;
    retain(
        &ask.out,
        "helper.txt",
        &format!(
            "path={}\nsha256={}\n",
            desk::HELPER_PATH,
            hash_guest(up, desk::HELPER_PATH)?
        ),
    )?;
    let n = std::process::id();
    let session = Session::launch_owned(up, Arch::Aarch64, n, &window::cpu_config(), ask.cpu)?;
    let scenario = Scenario {
        up,
        held,
        session: &session,
        out: &ask.out,
        native_release_unconfirmed: Cell::new(false),
    };
    let result = (|| {
        let identity = up.ssh.run(&format!("type {}\\identity.json", session.dir));
        if !identity.ok() {
            return Err("launched image identity could not be retained".into());
        }
        retain(&ask.out, "loaded-image.json", &identity.text)?;
        let version = scenario.ctl(&["version"])?;
        retain(&ask.out, "running-version.txt", &version)?;
        version_matches(&version, &ask.source)?;
        cases(&scenario)
    })();
    let diagnostics = retain(&ask.out, "stderr.txt", &session.stderr());
    held.check(&up.layout)
        .map_err(|e| format!("VM custody changed; owned cleanup refused: {e:?}"))?;
    session.end();
    let cleanup = session.ended();
    retain(
        &ask.out,
        "cleanup.txt",
        &format!(
            "owned_pid={}\nobserved_exit={:?}\nexact_image_gone={cleanup:?}\n",
            session.pid,
            session.exit()
        ),
    )?;
    cleanup?;
    diagnostics?;
    held.check(&up.layout).map_err(|e| format!("{e:?}"))?;
    if hash_guest(up, &format!("{bin}\\aterm-gui.exe"))? != gui
        || hash_guest(up, &format!("{bin}\\aterm-ctl.exe"))? != ctl
    {
        return Err("guest executable bytes changed during the observation; no verdict".into());
    }
    if result.is_ok() {
        session.remove_owned()?;
        retain(
            &ask.out,
            "owned-namespace.txt",
            "removed after exact image gone and confirmed evidence retention\n",
        )?;
    } else {
        retain(
            &ask.out,
            "owned-namespace.txt",
            &format!("retained for unconfirmed case evidence: {}\n", session.dir),
        )?;
    }
    result
}

fn version_matches(version: &str, source: &str) -> Result<(), String> {
    if let Some(commit) = version
        .split_whitespace()
        .find_map(|w| w.strip_prefix("commit="))
    {
        if commit.ends_with("-dirty") {
            return Err("known dirty app build cannot match a frozen clean source identity".into());
        }
        if (7..=40).contains(&commit.len())
            && commit.bytes().all(|b| b.is_ascii_hexdigit())
            && !source.starts_with(&commit.to_ascii_lowercase())
        {
            return Err("running app reports a different source commit".into());
        }
    }
    Ok(()) // Unknown remains unknown; executable hashes bind retained build evidence.
}

fn settings_view(tabs: &str) -> Option<String> {
    tabs.lines().find_map(|line| {
        let line = line.trim_start();
        if !line.starts_with("leaf ") {
            return None;
        }
        let fields = measure::fields(line);
        let field = |name: &str| {
            fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        if field("kind") != Some("app") || field("app") != Some("settings") {
            return None;
        }
        let view = field("view")?;
        (view.parse::<u64>().ok()? > 0).then(|| view.to_owned())
    })
}

fn focused(tree: &str, key: &str) -> bool {
    tree.lines().any(|line| {
        let fields = measure::fields(line.trim_start());
        fields.iter().any(|(k, v)| k == "key" && v == key)
            && fields
                .iter()
                .any(|(k, v)| k == "state" && v.split(',').any(|s| s == "focused=true"))
    })
}

fn named_whole(layout: &Layout, setting: &str) -> bool {
    let prefix = format!("settings/label/{setting}/");
    layout.nodes.iter().any(|node| {
        node.key.starts_with(&prefix)
            && Scenario::whole(layout, &node.key)
            && layout.texts.iter().any(|text| {
                text.key == node.key
                    && !text.painted.trim().is_empty()
                    && !text.overflow
                    && !text.clip_truncated
            })
    })
}

fn bool_control(tree: &str, key: &str) -> Option<bool> {
    tree.lines().find_map(|line| {
        let fields = measure::fields(line.trim_start());
        if !fields.iter().any(|(k, v)| k == "key" && v == key) {
            return None;
        }
        fields
            .iter()
            .find(|(k, _)| k == "value")
            .and_then(|(_, v)| v.parse().ok())
    })
}

fn button_state(text: &str) -> Option<bool> {
    match text
        .split_whitespace()
        .find_map(|word| word.strip_prefix("left_down="))?
    {
        "0" => Some(false),
        "1" => Some(true),
        _ => None,
    }
}

fn owned_button(text: &str, down: bool) -> bool {
    text.split_whitespace().any(|word| word == "target-owned=1") && button_state(text) == Some(down)
}

fn cases(s: &Scenario<'_>) -> Result<(), String> {
    let mut refused = Vec::new();
    for (name, case) in [
        (
            "cursor-history",
            history as fn(&Scenario<'_>) -> Result<(), String>,
        ),
        ("terminal-pointer", pointer),
        ("appearance-tab", tab),
    ] {
        if s.native_release_unconfirmed.get() {
            let why = "not attempted: native button is not confirmed released";
            retain(s.out, &format!("{name}-UNCONFIRMED.txt"), why)?;
            refused.push(format!("{name}: {why}"));
            continue;
        }
        if let Err(why) = case(s) {
            retain(s.out, &format!("{name}-UNCONFIRMED.txt"), &why)?;
            refused.push(format!("{name}: {why}"));
        }
    }
    retain(
        s.out,
        "OBSERVATIONS.txt",
        "Three bounded real-window cases scheduled; per-case records distinguish observations from cases not attempted. UNCONFIRMED means no acceptance verdict. Chrome-history and label/control association require review of retained geometry/pixels; this tool does not declare the library invariants proved. Application-control pointer is synthetic; native USB tablet/button and keyboard observations are separately labelled.\n",
    )?;
    if refused.is_empty() {
        Ok(())
    } else {
        Err(refused.join("\n"))
    }
}

fn history(s: &Scenario<'_>) -> Result<(), String> {
    let anchor = "settings/cursor-style/rainbow-kitty-underline";
    s.open("/cursor-styles")?;
    s.ctl(&["text-scale", "1"])?;
    s.viewport(318.0, 516.0)?;
    s.scroll_to(anchor)?;
    s.capture("history-1x", "application-control wheel")?;
    s.ctl(&["text-scale", "2"])?;
    let history = s.capture("history-2x", "application-control text-scale")?;
    // Route navigation reuses Settings state, so it cannot establish a fresh
    // chrome baseline. Destroy that owned tab and require a new view identity.
    let old = s.view()?;
    s.ctl(&["tab", "close"])?;
    s.open("/cursor-styles")?;
    if s.view()? == old {
        return Err("fresh Settings view was not created".into());
    }
    s.scroll_to(anchor)?;
    let fresh = s.capture("fresh-2x", "application-control navigation and wheel")?;
    if history.viewport != fresh.viewport || history.scale != fresh.scale {
        return Err("history and fresh native geometry differ".into());
    }
    // Retain both complete layouts/pixels: do not call unrelated semantic rows an
    // oracle for the private chrome plan, which inspection does not expose.
    Ok(())
}

fn pointer(s: &Scenario<'_>) -> Result<(), String> {
    s.open("/terminal")?;
    s.ctl(&["text-scale", "1"])?;
    s.viewport(584.0, 348.0)?;
    let layout = s.capture("terminal-before-pointer", "application-control navigation")?;
    let tree = s.ctl(&["inspect", "app/v1", "view", &s.view()?, "tree"])?;
    let native = native_pointer(s, &layout, &tree);
    if s.native_release_unconfirmed.get() {
        return native;
    }
    let (_, current) = s.layout()?;
    let xy = format!(
        "xy={},{}",
        current.viewport.x + current.viewport.w / 2.0,
        current.viewport.y + current.viewport.h / 2.0
    );
    s.ctl(&["pointer", "wheel", "dy=10000", "unit=pt", &xy])?;
    let last = s.capture(
        "terminal-last-screen",
        "application-control final manual scroll",
    )?;
    let color = "settings/control/search_match_color";
    if Scenario::whole(&last, color) {
        s.ctl(&["pointer", "move", "key=settings/control/search_match_color"])?;
        s.capture(
            "terminal-color-hover",
            "application-control synthetic pointer",
        )?;
    }
    // The last-screen pixels and labels must still be reviewed when the color
    // control is absent/inert; absence is not a positive pointer witness.
    native
}

fn native_pointer(s: &Scenario<'_>, layout: &Layout, tree: &str) -> Result<(), String> {
    // Select only an actual declared, whole, named Bool on this screen. No
    // platform field list or guessed last-row cut supplies a pointer witness.
    let (node, before) = layout
        .nodes
        .iter()
        .find_map(|node| {
            let setting = node.key.strip_prefix("settings/control/")?;
            (Scenario::whole(layout, &node.key) && named_whole(layout, setting))
                .then(|| bool_control(tree, &node.key).map(|value| (node, value)))
                .flatten()
        })
        .ok_or_else(|| {
            "no whole named Terminal Bool on actual screen; native pointer case unavailable"
                .to_string()
        })?;
    let place = window::desk(s.up, &["place", &s.session.pid.to_string(), ATERM_CLASS])?;
    let place = desk::parse_place(&place).ok_or("owned client origin unavailable")?;
    let monitors = window::desk(s.up, &["monitors"])?;
    let primary = desk::parse_monitors(&monitors)
        .into_iter()
        .find(|m| m.primary)
        .ok_or("primary monitor identity unavailable; no native pointer input")?;
    let x = place.at.0 + ((node.rect.x + node.rect.w / 2.0) * layout.scale).round() as i32;
    let y = place.at.1 + ((node.rect.y + node.rect.h / 2.0) * layout.scale).round() as i32;
    let r = primary.rect;
    if !s.only_window()?.foreground || x < r.l || x >= r.r || y < r.t || y >= r.b {
        return Err(
            "owned pointer target is not foreground/on primary; no native pointer input".into(),
        );
    }
    s.native_release_unconfirmed.set(true);
    let hit = s.hit(x, y)?;
    retain(s.out, "terminal-native-target.txt", &hit)?;
    if button_state(&hit) == Some(false) {
        s.native_release_unconfirmed.set(false);
    } else {
        // It may be somebody else's existing press: do not release it or send
        // later input while its state is held or unknown.
        s.native_release_unconfirmed.set(true);
    }
    if !owned_button(&hit, false) {
        return Err(
            "actual Win32 point/button is not owned, foreground and released; no native pointer input".into(),
        );
    }
    s.held.check(&s.up.layout).map_err(|e| format!("{e:?}"))?;
    let display = window::Display {
        primary: primary.clone(),
        second: primary,
    };
    s.native_release_unconfirmed.set(true);
    let held = window::press_to(&s.up.layout, &display, (x, y), (x, y), 0).and_then(|()| {
        let down_before = s.hit(x, y)?;
        let layout = s.capture("terminal-native-held", &format!("QMP USB tablet held screen={x},{y} key={} before={before}", node.key))?;
        let down_after = s.hit(x, y)?;
        retain(s.out, "terminal-native-held-target.txt", &format!("before_capture={down_before}\nafter_capture={down_after}\n"))?;
        if !owned_button(&down_before, true) || !owned_button(&down_after, true)
            || !s.whole_named_at(&layout, &node.key, x, y)? {
            return Err("held PointerDown did not retain the exact whole named control under its actual point".into());
        }
        Ok(())
    });
    // No capture/transport/I/O error above can skip this release attempt. A VM
    // identity change refuses dispatch to a different guest and remains unknown.
    let release = s
        .held
        .check(&s.up.layout)
        .map_err(|e| format!("release unavailable after custody change: {e:?}"))
        .and_then(|()| window::release(&s.up.layout));
    let up = s.hit(x, y);
    if up.as_ref().is_ok_and(|text| owned_button(text, false)) {
        s.native_release_unconfirmed.set(false);
    }
    let after = release.and_then(|()| {
        if s.native_release_unconfirmed.get() {
            return Err("native release lacks actual Win32 button-up/owned-target proof".into());
        }
        let layout = s.capture(
            "terminal-native-click",
            &format!(
                "QMP USB tablet released screen={x},{y} key={} before={before}",
                node.key
            ),
        )?;
        let tree = s.ctl(&["inspect", "app/v1", "view", &s.view()?, "tree"])?;
        if bool_control(&tree, &node.key) != Some(!before)
            || !s.whole_named_at(&layout, &node.key, x, y)?
        {
            return Err("native pointer did not flip the exact whole named Bool once".into());
        }
        let pointer = s.ctl(&["pointer", "status"])?;
        retain(s.out, "terminal-native-pointer-status.txt", &pointer)
    });
    retain(
        s.out,
        "terminal-native-release.txt",
        &format!(
            "held_error={:?}\npostrelease_error={:?}\nwin32_after_release={up:?}\n",
            held.as_ref().err(),
            after.as_ref().err()
        ),
    )?;
    match (held, after) {
        (Ok(()), Ok(())) => Ok(()),
        (held, after) => Err(format!("held={:?}; released={:?}", held.err(), after.err())),
    }
}

fn tab(s: &Scenario<'_>) -> Result<(), String> {
    s.open("/appearance")?;
    s.ctl(&["text-scale", "2"])?;
    s.viewport(1095.0, 696.0)?;
    s.scroll_to("settings/control/faint_opacity")?;
    s.capture("appearance-before-tab", "application-control wheel")?;
    // The target is deliberately Windows' available field, not the macOS opacity row.
    s.ctl(&["pointer", "move", "key=settings/control/faint_opacity"])?;
    s.ctl(&["pointer", "click"])?;
    let view = s.view()?;
    let tree = s.ctl(&["inspect", "app/v1", "view", &view, "tree"])?;
    if !focused(&tree, "settings/control/faint_opacity") {
        return Err("starting Appearance control did not receive focus; no USB Tab sent".into());
    }
    let owned = s.only_window()?;
    if !owned.foreground {
        return Err("owned window is not foreground; no USB input sent".into());
    }
    s.held.check(&s.up.layout).map_err(|e| format!("{e:?}"))?;
    window::chord(&s.up.layout, &["tab"])?;
    let after = s.capture(
        "appearance-after-tab",
        "QMP USB keyboard Tab (owned foreground verified)",
    )?;
    let tree = s.ctl(&["inspect", "app/v1", "view", &s.view()?, "tree"])?;
    if !focused(&tree, "settings/control/background_material")
        || !Scenario::whole(&after, "settings/control/background_material")
        || !Scenario::whole(&after, "settings/control/faint_opacity")
        || !named_whole(&after, "background_material")
    {
        return Err("Windows Appearance Tab did not land on the declared whole named control while retaining its predecessor".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_and_released_input_require_actual_owned_button_observations() {
        assert!(owned_button("target-owned=1 left_down=1", true));
        assert!(owned_button("target-owned=1 left_down=0", false));
        assert!(!owned_button("target-owned=1 left_down=0", true));
        assert!(!owned_button("target-owned=0 left_down=1", true));
        assert!(!owned_button("target-owned=1", false));
        assert!(!owned_button("target-owned=1 left_down=unknown", false));
        assert!(!owned_button("not-target-owned=1 left_down=1", true));
    }

    #[test]
    fn only_a_settings_app_leaf_with_a_retained_view_identifies_the_target() {
        let leaf =
            "    leaf path=/ view=42 rect_px=0,20,584,348 focused=true kind=app app=settings";
        assert_eq!(settings_view(leaf).as_deref(), Some("42"));
        for other in [
            leaf.replace("app=settings", "app=workers"),
            leaf.replace("kind=app", "kind=terminal"),
            leaf.replace("view=42", "view=unknown"),
            leaf.replace("view=42", "view=0"),
            leaf.replace("leaf ", "tab "),
        ] {
            assert_eq!(settings_view(&other), None);
        }
    }

    #[test]
    fn focus_witness_requires_the_exact_control_and_true_field() {
        let tree = "node key=\"settings/control/background_material\" role=Combobox state=enabled=true,focused=true,focus-visible=true\n";
        assert!(focused(tree, "settings/control/background_material"));
        assert!(!focused(tree, "settings/control/background_opacity"));
        assert!(!focused(
            &tree.replace("focused=true", "focused=false"),
            "settings/control/background_material"
        ));
        assert!(!focused(
            "node key=\"settings/control/background_material\" label=\"focused=true\" state=-",
            "settings/control/background_material"
        ));
    }

    #[test]
    fn settings_observation_requires_explicit_frozen_binary_identities() {
        let args = vec![
            "--source".into(),
            "a".repeat(40),
            "--gui-sha".into(),
            "b".repeat(64),
            "--ctl-sha".into(),
            "c".repeat(64),
            "--backend".into(),
            "cpu".into(),
            "--out".into(),
            "/tmp/.ubd/settings.noindex".into(),
        ];
        assert!(parse(&args).is_ok());
        for (index, invalid) in [
            (1, "unknown"),
            (3, "unverified"),
            (5, ""),
            (7, "auto"),
            (9, "relative"),
        ] {
            let mut changed = args.clone();
            changed[index] = invalid.into();
            assert!(parse(&changed).is_err());
        }
    }

    #[test]
    fn unknown_version_does_not_become_source_proof_or_known_mismatch() {
        let source = "1234567890abcdef1234567890abcdef12345678";
        assert!(version_matches("OK commit=1234567890ab", source).is_ok());
        assert!(version_matches("OK commit=unknown", source).is_ok());
        assert!(version_matches("OK version=unknown", source).is_ok());
        assert!(version_matches("OK commit=abcdef012345", source).is_err());
        assert!(version_matches("OK commit=1234567890ab-dirty", source).is_err());
    }

    #[test]
    fn bool_witness_is_bound_to_exact_semantic_identity() {
        let tree = "node key=\"settings/control/a\" role=Switch value=false state=enabled=true\nnode key=\"settings/control/b\" role=Switch value=true state=enabled=true\n";
        assert_eq!(bool_control(tree, "settings/control/a"), Some(false));
        assert_eq!(bool_control(tree, "settings/control/b"), Some(true));
        assert_eq!(bool_control(tree, "settings/control/missing"), None);
        assert_eq!(
            bool_control(
                "node key=\"settings/control/a\" value=-",
                "settings/control/a"
            ),
            None
        );
    }
}
