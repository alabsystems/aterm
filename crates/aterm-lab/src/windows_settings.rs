// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Bounded real Settings observations under explicit, non-powering Windows custody.
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
    let path = dir.join(name);
    let mut file = crate::util::create_new_regular_file(&path, 0o600)
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
    geometry_call: Cell<u32>,
    scroll_call: Cell<u32>,
}

impl Scenario<'_> {
    fn hit(&self, x: i32, y: i32) -> Result<String, String> {
        self.held
            .check(&self.up.layout)
            .map_err(|e| format!("{e:?}"))?;
        let (birth, image) = self.session.owned_identity()?;
        window::desk(
            self.up,
            &[
                "hit-owned",
                &self.session.pid.to_string(),
                birth,
                image,
                ATERM_CLASS,
                &x.to_string(),
                &y.to_string(),
            ],
        )
    }

    fn whole_named_at(
        &self,
        layout: &Layout,
        key: &str,
        x: i32,
        y: i32,
        mapping: NativeMapping,
        label: &str,
    ) -> Result<bool, String> {
        if layout.scale != 1.0 || self.view()?.parse::<u64>().ok() != Some(mapping.view) {
            return Err("native inverse mapping view/scale changed".into());
        }
        let placed = window::desk(
            self.up,
            &["place", &self.session.pid.to_string(), ATERM_CLASS],
        )?;
        let placed = desk::parse_place(&placed).ok_or("client origin unavailable")?;
        let raw = self.observed_ctl(label, &["pointer", "status"])?;
        let current = native_mapping(&raw, &mapping.view.to_string(), key, layout.scale)?;
        let point = mapping
            .inverse((x, y), placed.at)
            .ok_or("native inverse mapping overflow")?;
        if current.origin != mapping.origin
            || current.screen(placed.at)? != (x, y)
            || current.logical != point
        {
            return Ok(false);
        }
        Ok(Scenario::whole(layout, key)
            && layout.nodes.iter().any(|node| {
                node.key == key && node.rect.contains(f64::from(point.0), f64::from(point.1))
            })
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

    fn observed_ctl(&self, label: &str, words: &[&str]) -> Result<String, String> {
        self.held
            .check(&self.up.layout)
            .map_err(|e| format!("{e:?}"))?;
        self.session.ctl_observed(words, |code, raw| {
            retain(
                self.out,
                &format!("{label}.txt"),
                &format!("request={}\nexit={code:?}\n{raw}", words.join(" ")),
            )
        })
    }

    fn optional_ctl(&self, label: &str, words: &[&str]) -> Result<(), String> {
        self.held
            .check(&self.up.layout)
            .map_err(|e| format!("{e:?}"))?;
        let mut recorded = false;
        let answer = self.session.ctl_observed(words, |code, raw| {
            retain(
                self.out,
                &format!("{label}.txt"),
                &format!("request={}\nexit={code:?}\n{raw}", words.join(" ")),
            )?;
            recorded = true;
            Ok(())
        });
        if !recorded {
            return Err(answer
                .err()
                .unwrap_or_else(|| "command receipt was not retained".into()));
        }
        if let Err(error) = answer {
            retain(
                self.out,
                &format!("{label}-unavailable.txt"),
                &format!(
                    "diagnostic=Unavailable\nrequest={}\nreason={error}\n",
                    words.join(" ")
                ),
            )?;
        }
        Ok(())
    }

    fn geometry(&self, label: &str) -> Result<(Layout, desk::Window, GridObservation), String> {
        let tabs = self.observed_ctl(&format!("{label}-tabs"), &["inspect", "app/v1", "tabs"])?;
        let view = settings_view(&tabs).ok_or("no actual Settings view")?;
        let audit = self.observed_ctl(
            &format!("{label}-audit"),
            &["inspect", "app/v1", "view", &view, "audit"],
        )?;
        let layout = measure::parse_layout(&audit)?;
        self.observed_ctl(
            &format!("{label}-tree"),
            &["inspect", "app/v1", "view", &view, "tree"],
        )?;
        for (name, words) in [
            ("dims", vec!["dims"]),
            ("status", vec!["status"]),
            ("resizes", vec!["resizes"]),
            ("text-scale", vec!["text-scale"]),
        ] {
            self.optional_ctl(&format!("{label}-{name}"), &words)?;
        }
        self.held
            .check(&self.up.layout)
            .map_err(|e| format!("{e:?}"))?;
        let raw = window::desk(self.up, &["windows", &self.session.pid.to_string()])?;
        retain(self.out, &format!("{label}-win32.txt"), &raw)?;
        let mut owned = desk::parse_windows(&raw)
            .into_iter()
            .filter(|w| w.class == ATERM_CLASS);
        let owned_window = owned.next().ok_or("no owned visible app window")?;
        if owned.next().is_some() {
            return Err("multiple owned app windows; target is ambiguous".into());
        }
        let monitors = window::desk(self.up, &["monitors"])?;
        retain(self.out, &format!("{label}-monitors.txt"), &monitors)?;
        retain(
            self.out,
            &format!("{label}-logical.txt"),
            &format!(
                "viewport={:?}\nscale={}\nwindow={owned_window:?}\n",
                layout.viewport, layout.scale
            ),
        )?;
        let identity = grid_observation(&tabs, &audit)?;
        Ok((layout, owned_window, identity))
    }

    fn place_on_existing_monitor(&self, case: &str, label: &str) -> Result<(), String> {
        self.held
            .check(&self.up.layout)
            .map_err(|e| format!("{e:?}"))?;
        let before = self.only_window()?;
        let raw = window::desk(self.up, &["monitors"])?;
        retain(self.out, &format!("{label}-placement-monitors.txt"), &raw)?;
        let pointer = case == "terminal-pointer";
        let monitor = desk::parse_monitors(&raw)
            .into_iter()
            .find(|monitor| {
                monitor.primary == pointer
                    && monitor.dpi == before.dpi
                    && before.rect.width() <= monitor.rect.width()
                    && before.rect.height() <= monitor.rect.height()
            })
            .ok_or("no same-DPI existing monitor can contain the owned case window")?;
        let x = monitor.rect.l + (monitor.rect.width() - before.rect.width()) / 2;
        let y = monitor.rect.t + (monitor.rect.height() - before.rect.height()) / 2;
        let (birth, image) = self.session.owned_identity()?;
        let moved = window::desk(
            self.up,
            &[
                "move-owned",
                &self.session.pid.to_string(),
                birth,
                image,
                ATERM_CLASS,
                &x.to_string(),
                &y.to_string(),
                &before.dpi.to_string(),
            ],
        )?;
        retain(
            self.out,
            &format!("{label}-placement-move.txt"),
            &format!(
                "case={case}\npointer_case_primary_only={pointer}\nbefore={before:?}\nselected={monitor:?}\n{moved}"
            ),
        )?;
        if !moved.trim().starts_with("owned-move hwnd=") {
            return Err("exact owned-window placement was not acknowledged".into());
        }
        let after = self.only_window()?;
        retain(
            self.out,
            &format!("{label}-placement-after.txt"),
            &format!("{after:?}\n"),
        )?;
        if after.dpi != before.dpi
            || after.rect.width() != before.rect.width()
            || after.rect.height() != before.rect.height()
            || after.rect.l != x
            || after.rect.t != y
            || !monitor_contains(monitor.rect, after.rect)
        {
            return Err(
                "owned window is not wholly contained on its selected same-DPI monitor".into(),
            );
        }
        Ok(())
    }

    fn grid_viewport(
        &self,
        case: &str,
        original: (u16, u16),
        declared: (u16, u16),
    ) -> Result<(), String> {
        let projected = projected_grid(original.0, original.1)?;
        if projected != declared {
            return Err("declared grid differs from the explicit ceiling projection".into());
        }
        let (rows, cols) = declared;
        let width = f64::from(cols) * 9.0 + 24.0;
        let height = f64::from(rows) * 23.0 + 12.0;
        let call = self.geometry_call.get();
        self.geometry_call.set(
            call.checked_add(1)
                .ok_or("geometry record sequence exhausted")?,
        );
        retain(
            self.out,
            &format!("grid-{call}-case.txt"),
            &format!(
                "case={case}\noriginal_exact_pixels={}x{}\noriginal_physical_acceptance=UNREPRESENTABLE\nqualification_kind=explicit-reachable-grid\ndeclared_rows={rows}\ndeclared_cols={cols}\nsource_bound_cell_px=9x23\nsource_bound_pad_px=12\nsource_bound_device_scale=1\nceil_projection=(height-12)/23,(width-24)/9\nprojected_logical={width}x{height}\n",
                original.0, original.1
            ),
        )?;
        let (before, _, identity) = self.geometry(&format!("grid-{call}-before"))?;
        if before.scale != 1.0
            || before.viewport.w != f64::from(identity.cols) * 9.0 + 24.0
            || before.viewport.h != f64::from(identity.rows) * 23.0 + 12.0
        {
            return Err(
                "actual source-bound cell/padding metrics differ; explicit grid case unavailable"
                    .into(),
            );
        }
        self.observed_ctl(
            &format!("grid-{call}-request"),
            &["resize", &rows.to_string(), &cols.to_string()],
        )?;
        let changed = (identity.rows, identity.cols) != declared;
        let mut poll = 0u32;
        let mut fatal = None;
        let matched = window::within("declared grid and presented native geometry", || {
            let label = format!("grid-{call}-observed-{poll}");
            poll += 1;
            let observed = match self.geometry(&label) {
                Ok(value) => value,
                Err(error) => {
                    fatal = Some(error);
                    return Ok(None);
                }
            };
            let (layout, _, now) = &observed;
            let ready = (now.rows, now.cols) == declared
                && layout.scale == 1.0
                && layout.viewport.w == width
                && layout.viewport.h == height
                && now.presented
                && now.current
                && (!changed
                    || (now.geometry != identity.geometry && now.capture > identity.capture))
                && now.presentation >= identity.presentation;
            if ready {
                Ok(Some(observed))
            } else {
                Err(format!(
                    "waiting for declared grid={declared:?}, observed={now:?}, viewport={:?}",
                    layout.viewport
                ))
            }
        })?;
        if let Some(error) = fatal {
            return Err(error);
        }
        matched.ok_or_else(|| "required geometry observation unavailable".to_string())?;
        self.place_on_existing_monitor(case, &format!("grid-{call}"))?;
        let (placed, _, placed_grid) = self.geometry(&format!("grid-{call}-placed"))?;
        if (placed_grid.rows, placed_grid.cols) != declared
            || placed.viewport.w != width
            || placed.viewport.h != height
            || placed.scale != 1.0
            || !placed_grid.presented
            || !placed_grid.current
        {
            return Err("native geometry changed after same-DPI owned placement".into());
        }
        Ok(())
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
        let context = format!("{label}-context");
        let (layout, owned, identity) = self.geometry(&context)?;
        if !identity.presented || !identity.current {
            return Err("native capture does not describe current presented geometry".into());
        }
        self.optional_ctl(&format!("{label}-metrics"), &["metrics"])?;
        let mut text = format!(
            "input_transport={transport}\nrequired_inspection_context={context}\nidentity={identity:?}\nviewport={:?}\nscale={}\n",
            layout.viewport, layout.scale
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
        aterm_uds::regular_file::open(&local)
            .and_then(|(f, _)| f.set_permissions(std::fs::Permissions::from_mode(0o600)))
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
        let call = self.scroll_call.get();
        self.scroll_call.set(
            call.checked_add(1)
                .ok_or("scroll record sequence exhausted")?,
        );
        let mut walk = format!(
            "target={key}\nmax_manual_inputs=32\nmanual_input_protocol=one-semantic-line-per-input\nsource_verb=pointer wheel dy=1 unit=lines\n"
        );
        // Each input is a distinct manual scroll, not a retry of a failed check.
        for step in 0..32 {
            let (_, layout) = self.layout()?;
            self.optional_ctl(
                &format!("scroll-{call}-{step}-prefs"),
                &["controls", "prefs"],
            )?;
            walk.push_str(&format!(
                "step={step} viewport={:?} target_paint={:?}\n",
                layout.viewport,
                layout.nodes.iter().find(|node| node.key == key)
            ));
            if Self::whole(&layout, key) {
                retain(self.out, &format!("scroll-{call}-walk.txt"), &walk)?;
                return Ok(());
            }
            let xy = format!(
                "xy={},{}",
                layout.viewport.x + layout.viewport.w / 2.0,
                layout.viewport.y + layout.viewport.h / 2.0
            );
            self.ctl(&["pointer", "wheel", "dy=1", "unit=lines", &xy])?;
            self.settled()?;
        }
        retain(self.out, &format!("scroll-{call}-walk.txt"), &walk)?;
        let diagnostic = self.capture(
            &format!("scroll-{call}-unreached"),
            "bounded application-control wheel walk exhausted; no focus or keyboard input",
        );
        Err(format!(
            "{key} not whole after bounded manual scroll walk; final_capture={:?}",
            diagnostic.as_ref().map(|_| ()).map_err(String::as_str)
        ))
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
        geometry_call: Cell::new(0),
        scroll_call: Cell::new(0),
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

fn cases(s: &Scenario<'_>) -> Result<(), String> {
    let mut refused = Vec::new();
    for (name, case) in [
        (
            "cursor-history",
            history as fn(&Scenario<'_>) -> Result<(), String>,
        ),
        ("sound-value", sound),
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
        "Four bounded real-window cases scheduled; per-case records distinguish observations from cases not attempted. UNCONFIRMED means no acceptance verdict. Chrome-history and label/control association require review of retained geometry/pixels; this tool does not declare the library invariants proved. Application-control pointer and Sound PageDown are synthetic application-control input; native USB tablet/button and keyboard observations are separately labelled.\n",
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
    s.grid_viewport("cursor-history", (318, 516), (22, 33))?;
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

fn sound(s: &Scenario<'_>) -> Result<(), String> {
    let key = "settings/control/trail_sound_style";
    s.open("/sound")?;
    s.ctl(&["text-scale", "2"])?;
    s.grid_viewport("sound-value", (278, 264), (11, 29))?;
    let mut walk = String::from(
        "case=sound-value\nsupported_minimum=278x264\ndeclared_grid=29cols11rows\nprojected_viewport=285x265\ntext_scale=2\ninput_transport=application-control key pagedown\nmax_manual_inputs=32\nprivate_trail_sound_style=default\nstress_164x98_physical_scope=NOT_EXERCISED\nno_choice_or_audio_action=true\n",
    );
    retain(s.out, "sound-value-contract.txt", &walk)?;
    for step in 0..=32 {
        s.settled()?;
        let scale = s.observed_ctl(&format!("sound-step-{step}-text-scale"), &["text-scale"])?;
        let fields = measure::fields(scale.trim());
        if one_field(&fields, "text_scale").and_then(|value| value.parse::<f64>().ok()) != Some(2.0)
            || one_field(&fields, "source") != Some("injected")
        {
            return Err("Sound text scale is not observed as injected2x".into());
        }
        let (layout, _, identity) = s.geometry(&format!("sound-step-{step}"))?;
        if !identity.current
            || !identity.presented
            || (identity.rows, identity.cols) != (11, 29)
            || layout.scale != 1.0
            || layout.viewport.w != 285.0
            || layout.viewport.h != 265.0
        {
            return Err("Sound observation lost its declared current presented geometry".into());
        }
        s.optional_ctl(&format!("sound-step-{step}-prefs"), &["controls", "prefs"])?;
        let tabs = s.observed_ctl(
            &format!("sound-step-{step}-tabs"),
            &["inspect", "app/v1", "tabs"],
        )?;
        let view = settings_view(&tabs).ok_or("Sound Settings view unavailable")?;
        let tree = s.observed_ctl(
            &format!("sound-step-{step}-tree"),
            &["inspect", "app/v1", "view", &view, "tree"],
        )?;
        let semantic = grid_observation(&tabs, &tree)?;
        if !semantic.current
            || !semantic.presented
            || semantic.view != identity.view
            || semantic.window != identity.window
            || semantic.generation != identity.generation
            || semantic.geometry != identity.geometry
            || semantic.fingerprint != identity.fingerprint
        {
            return Err("Sound semantics do not match the current presented observation".into());
        }
        let semantic_nodes = tree
            .lines()
            .map(|line| measure::fields(line.trim_start()))
            .filter(|fields| one_field(fields, "key") == Some(key))
            .collect::<Vec<_>>();
        let meaning = match semantic_nodes.as_slice() {
            [fields] => one_field(fields, "label").zip(one_field(fields, "value")),
            _ => None,
        };
        let control = layout.nodes.iter().find(|node| node.key == key);
        let texts = layout
            .texts
            .iter()
            .filter(|text| text.key == key)
            .collect::<Vec<_>>();
        walk.push_str(&format!("step={step} viewport={:?} control={control:?} value_texts={texts:?} semantic_meaning={meaning:?}\n", layout.viewport));
        retain(s.out, &format!("sound-step-{step}-observation.txt"), &walk)?;
        let value = texts
            .iter()
            .map(|text| text.painted.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let value = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let observed = Scenario::whole(&layout, key)
            && named_whole(&layout, "trail_sound_style")
            && control.is_some_and(|node| {
                (node.kind == "button" || node.role.eq_ignore_ascii_case("combobox"))
                    && meaning.is_some_and(|(label, value)| {
                        label == node.label
                            && label.contains("(default)")
                            && value.contains("(default)")
                    })
            })
            && !value.is_empty()
            && texts
                .iter()
                .all(|text| !text.overflow && !text.clip_truncated);
        if observed {
            let captured = s.capture("sound-value-whole", "application-control PageDown")?;
            if captured != layout {
                return Err("Sound value changed between observation and retained pixels".into());
            }
            walk.push_str(&format!(
                "observed_visual_value={value:?}\nobserved_semantic_label={:?}\n",
                control.map(|node| &node.label)
            ));
            retain(s.out, "sound-value-walk.txt", &walk)?;
            return Ok(());
        }
        if step == 32 {
            break;
        }
        s.observed_ctl(&format!("sound-step-{step}-pagedown"), &["key", "pagedown"])?;
    }
    retain(s.out, "sound-value-walk.txt", &walk)?;
    let captured = s.capture(
        "sound-value-unreached",
        "bounded application-control PageDown exhausted",
    );
    Err(format!(
        "Sound value/name/default semantics not observed whole after32 PageDown inputs; final_capture={:?}",
        captured.as_ref().map(|_| ()).map_err(String::as_str)
    ))
}

fn pointer(s: &Scenario<'_>) -> Result<(), String> {
    s.open("/terminal")?;
    s.ctl(&["text-scale", "1"])?;
    s.grid_viewport("terminal-pointer", (584, 348), (15, 63))?;
    s.capture("terminal-before-pointer", "application-control navigation")?;
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
    let view = s.view()?;
    let tabs = s.observed_ctl("terminal-native-tabs", &["inspect", "app/v1", "tabs"])?;
    let audit = s.observed_ctl(
        "terminal-native-audit",
        &["inspect", "app/v1", "view", &view, "audit"],
    )?;
    let tree = s.observed_ctl(
        "terminal-native-tree",
        &["inspect", "app/v1", "view", &view, "tree"],
    )?;
    let painted = grid_observation(&tabs, &audit)?;
    let semantic = grid_observation(&tabs, &tree)?;
    let current = measure::parse_layout(&audit)?;
    if !painted.presented
        || !painted.current
        || !semantic.presented
        || !semantic.current
        || painted.geometry != semantic.geometry
        || painted.fingerprint != semantic.fingerprint
        || current != last
    {
        return Err("native pointer semantics do not match the captured last-screen paint".into());
    }
    let native = native_pointer(s, &current, &tree, painted);
    if s.native_release_unconfirmed.get() {
        return native;
    }
    let hover = (|| {
        let (_, after_native) = s.layout()?;
        let color = "settings/control/search_match_color";
        if Scenario::whole(&after_native, color) {
            s.ctl(&["pointer", "move", "key=settings/control/search_match_color"])?;
            s.capture(
                "terminal-color-hover",
                "application-control synthetic pointer",
            )?;
        }
        Ok(())
    })();
    // The last-screen pixels and labels must still be reviewed when the color
    // control is absent/inert; absence is not a positive pointer witness.
    native.and(hover)
}

fn native_pointer(
    s: &Scenario<'_>,
    layout: &Layout,
    tree: &str,
    identity: GridObservation,
) -> Result<(), String> {
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

    s.native_release_unconfirmed.set(true);
    if layout.scale != 1.0 {
        return Err("native mapping case requires the observed scale-one viewport".into());
    }
    let place_raw = window::desk(s.up, &["place", &s.session.pid.to_string(), ATERM_CLASS])?;
    retain(s.out, "terminal-native-client-before.txt", &place_raw)?;
    let place = desk::parse_place(&place_raw).ok_or("owned client origin unavailable")?;
    let owned_before = s.only_window()?;
    let monitors = window::desk(s.up, &["monitors"])?;
    let primary = desk::parse_monitors(&monitors)
        .into_iter()
        .find(|monitor| monitor.primary)
        .ok_or("primary monitor identity unavailable; no native pointer input")?;
    if !owned_before.foreground || !monitor_contains(primary.rect, owned_before.rect) {
        return Err("owned pointer window is not foreground and wholly on primary".into());
    }
    let center = (
        place
            .at
            .0
            .checked_add(i32::try_from(place.client.0 / 2).map_err(|e| e.to_string())?),
        place
            .at
            .1
            .checked_add(i32::try_from(place.client.1 / 2).map_err(|e| e.to_string())?),
    );
    let center = (
        center.0.ok_or("client center overflow")?,
        center.1.ok_or("client center overflow")?,
    );
    let initial = s.hit(center.0, center.1)?;
    retain(s.out, "terminal-native-before-synthetic.txt", &initial)?;
    if !released_owned_target(&initial) {
        return Err(
            "owned foreground/button-up proof unavailable before synthetic mapping; no input"
                .into(),
        );
    }
    let logical = (
        (node.rect.x + node.rect.w / 2.0).round(),
        (node.rect.y + node.rect.h / 2.0).round(),
    );
    let logical = (
        integer_coordinate(&logical.0.to_string()).ok_or("invalid whole-control logical x")?,
        integer_coordinate(&logical.1.to_string()).ok_or("invalid whole-control logical y")?,
    );
    if !node
        .rect
        .contains(f64::from(logical.0), f64::from(logical.1))
    {
        return Err("integer mapping point is outside the whole control".into());
    }
    let xy = format!("xy={},{}", logical.0, logical.1);
    // This changes only the application's synthetic pointer. It is a mapping
    // precursor, never proof of OS cursor delivery or native PointerDown.
    s.observed_ctl(
        "terminal-native-synthetic-map-move",
        &["pointer", "move", &xy],
    )?;
    let view = s.view()?;
    if view.parse::<u64>().ok() != Some(identity.view) {
        return Err("native view changed before mapping".into());
    }
    let mut mapping_fatal = None;
    let mut mapping_poll = 0u32;
    let mapping = window::within("authoritative synthetic native mapping status", || {
        let label = format!("terminal-native-synthetic-map-status-{mapping_poll}");
        mapping_poll += 1;
        let raw = match s.observed_ctl(&label, &["pointer", "status"]) {
            Ok(raw) => raw,
            Err(error) => {
                mapping_fatal = Some(error);
                return Ok(None);
            }
        };
        match native_mapping(&raw, &view, &node.key, layout.scale) {
            Ok(mapping) if mapping.logical == logical => Ok(Some(mapping)),
            Ok(_) => Err(
                "known native mapping has not reached the single requested logical point".into(),
            ),
            Err(error) => {
                mapping_fatal = Some(error);
                Ok(None)
            }
        }
    })?;
    if let Some(error) = mapping_fatal {
        return Err(error);
    }
    let mapping = mapping.ok_or("native coordinate mapping unavailable")?;
    retain(
        s.out,
        "terminal-native-synthetic-map-transform.txt",
        &format!(
            "evidence_kind=SYNTHETIC_application_coordinate_mapping_NOT_native_input_proof\nreturned_mapping={mapping:?}\nrequested_logical={logical:?}\nprehover_identity={identity:?}\n"
        ),
    )?;
    let tabs = s.observed_ctl(
        "terminal-native-post-map-tabs",
        &["inspect", "app/v1", "tabs"],
    )?;
    let audit = s.observed_ctl(
        "terminal-native-post-map-audit",
        &["inspect", "app/v1", "view", &view, "audit"],
    )?;
    let tree = s.observed_ctl(
        "terminal-native-post-map-tree",
        &["inspect", "app/v1", "view", &view, "tree"],
    )?;
    let painted = grid_observation(&tabs, &audit)?;
    let semantic = grid_observation(&tabs, &tree)?;
    let post = measure::parse_layout(&audit)?;
    if !painted.presented
        || !painted.current
        || !semantic.presented
        || !semantic.current
        || painted.view != identity.view
        || painted.window != identity.window
        || painted.generation != identity.generation
        || semantic.view != painted.view
        || semantic.window != painted.window
        || semantic.generation != painted.generation
        || painted.geometry != identity.geometry
        || semantic.geometry != painted.geometry
        || painted.fingerprint != semantic.fingerprint
        || post.scale != 1.0
        || post.viewport != layout.viewport
        || !Scenario::whole(&post, &node.key)
        || !named_whole(
            &post,
            node.key
                .strip_prefix("settings/control/")
                .ok_or("not a declared control")?,
        )
        || bool_control(&tree, &node.key) != Some(before)
        || !post.nodes.iter().any(|current| {
            current.key == node.key
                && current.rect == node.rect
                && current
                    .rect
                    .contains(f64::from(mapping.logical.0), f64::from(mapping.logical.1))
        })
    {
        return Err("current presented whole Bool no longer matches the post-hover mapping".into());
    }
    let status = s.observed_ctl("terminal-native-post-map-status", &["pointer", "status"])?;
    let rebound = native_mapping(&status, &view, &node.key, post.scale)?;
    let after_place_raw = window::desk(s.up, &["place", &s.session.pid.to_string(), ATERM_CLASS])?;
    retain(
        s.out,
        "terminal-native-client-after-map.txt",
        &after_place_raw,
    )?;
    let after_place =
        desk::parse_place(&after_place_raw).ok_or("owned client placement unavailable")?;
    let owned_after = s.only_window()?;
    if rebound != mapping
        || after_place != place
        || owned_after.rect != owned_before.rect
        || owned_after.dpi != owned_before.dpi
        || !owned_after.foreground
    {
        return Err(
            "native coordinate mapping or owned window placement changed after hover".into(),
        );
    }
    let (x, y) = mapping.screen(place.at)?;
    if !primary.rect.holds((x, y)) {
        return Err("mapped native target is outside primary".into());
    }
    let hit = s.hit(x, y)?;
    retain(s.out, "terminal-native-target.txt", &hit)?;
    if !released_owned_target(&hit) {
        return Err("mapped owned target/button-up proof unavailable before OS move".into());
    }
    s.held.check(&s.up.layout).map_err(|e| format!("{e:?}"))?;
    let display = window::Display {
        primary: primary.clone(),
        second: primary,
    };
    s.held.check(&s.up.layout).map_err(|e| format!("{e:?}"))?;
    s.native_release_unconfirmed.set(true);
    window::move_tablet(&s.up.layout, &display, (x, y))?;
    let mut movement_fatal = None;
    let mut movement_poll = 0u32;
    window::within(
        "actual Win32 cursor at the owned named control with button up",
        || {
            let reading = s.hit(x, y);
            let label = format!("terminal-native-moved-{movement_poll}.txt");
            movement_poll += 1;
            let raw = match reading {
                Ok(raw) => raw,
                Err(error) => {
                    movement_fatal = Some(error);
                    return Ok(());
                }
            };
            if let Err(error) = retain(s.out, &label, &raw) {
                movement_fatal = Some(error);
                return Ok(());
            }
            match cursor_ready_to_press(&raw, (x, y)) {
                Ok(true) => Ok(()),
                Ok(false) => Err(
                    "known released cursor is still moving within its owned foreground window"
                        .into(),
                ),
                Err(error) => {
                    movement_fatal = Some(error);
                    Ok(())
                }
            }
        },
    )?;
    if let Some(error) = movement_fatal {
        return Err(error);
    }
    s.held.check(&s.up.layout).map_err(|e| format!("{e:?}"))?;
    let predown = s.hit(x, y)?;
    retain(s.out, "terminal-native-before-down.txt", &predown)?;
    if !actual_cursor_button(&predown, (x, y), false) {
        return Err("actual cursor ownership/button-up changed before PointerDown".into());
    }
    s.native_release_unconfirmed.set(true);
    let held = window::tablet_down(&s.up.layout).and_then(|()| {
        let down_before = s.hit(x, y)?;
        let layout = s.capture("terminal-native-held", &format!("QMP USB tablet held screen={x},{y} key={} before={before}", node.key))?;
        let down_after = s.hit(x, y)?;
        retain(s.out, "terminal-native-held-target.txt", &format!("before_capture={down_before}\nafter_capture={down_after}\n"))?;
        if !actual_cursor_button(&down_before, (x, y), true) || !actual_cursor_button(&down_after, (x, y), true)
            || !s.whole_named_at(&layout, &node.key, x, y, mapping, "terminal-native-held-app-pointer")? {
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
    if up
        .as_ref()
        .is_ok_and(|text| actual_cursor_button(text, (x, y), false))
    {
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
            || !s.whole_named_at(
                &layout,
                &node.key,
                x,
                y,
                mapping,
                "terminal-native-released-app-pointer",
            )?
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
    s.grid_viewport("appearance-tab", (1095, 696), (30, 119))?;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridObservation {
    rows: u16,
    cols: u16,
    view: u64,
    window: u64,
    generation: u64,
    geometry: u64,
    fingerprint: u64,
    capture: u64,
    presentation: u64,
    presented: bool,
    current: bool,
}

fn one_field<'a>(fields: &'a [(String, String)], key: &str) -> Option<&'a str> {
    let mut matches = fields.iter().filter(|(name, _)| name == key);
    let value = matches.next()?.1.as_str();
    matches.next().is_none().then_some(value)
}

fn grid_observation(tabs: &str, audit: &str) -> Result<GridObservation, String> {
    use aterm_types::app_inspection::{InspectRequest, validate_inspection_response};
    validate_inspection_response(tabs, InspectRequest::Tabs).map_err(|e| e.to_string())?;
    let mut windows = tabs.lines().filter_map(|line| line.strip_prefix("window "));
    let fields = measure::fields(windows.next().ok_or("missing window grid")?);
    if windows.next().is_some() {
        return Err("ambiguous window grid".into());
    }
    let mut sources = audit
        .lines()
        .filter_map(|line| line.strip_prefix("inspection-source "));
    let source = measure::fields(sources.next().ok_or("missing inspection source")?);
    if sources.next().is_some() {
        return Err("ambiguous inspection source".into());
    }
    let parse = || -> Option<GridObservation> {
        let rows = one_field(&fields, "rows")?.parse::<u16>().ok()?;
        let cols = one_field(&fields, "cols")?.parse::<u16>().ok()?;
        if rows == 0 || cols == 0 {
            return None;
        }
        let view = one_field(&source, "view")?
            .parse::<u64>()
            .ok()
            .filter(|view| *view > 0)?;
        let window = one_field(&source, "window")?.parse::<u64>().ok()?;
        if window != one_field(&fields, "id")?.parse::<u64>().ok()? {
            return None;
        }
        Some(GridObservation {
            rows,
            cols,
            view,
            window,
            generation: one_field(&source, "generation")?.parse().ok()?,
            geometry: u64::from_str_radix(one_field(&source, "geometry")?, 16).ok()?,
            fingerprint: u64::from_str_radix(one_field(&source, "compiled-fingerprint")?, 16)
                .ok()?,
            capture: one_field(&source, "capture-serial")?.parse().ok()?,
            presentation: one_field(&source, "presentation-revision")?.parse().ok()?,
            presented: one_field(&source, "source")? == "presented",
            current: one_field(&source, "model-current")? == "true",
        })
    };
    parse().ok_or_else(|| "invalid grid or inspection identity".into())
}

fn projected_grid(width: u16, height: u16) -> Result<(u16, u16), String> {
    // Source-bound physical metrics observed in all fifteen frozen-app snapshots:
    // cw=9, ch=23, pad=12, scale=1. This is an explicit qualification case,
    // never a replacement acceptance claim for the original off-lattice pixels.
    if width <= 24 || height <= 12 {
        return Err("original viewport is below the projection domain".into());
    }
    Ok(((height - 12).div_ceil(23), (width - 24).div_ceil(9)))
}

#[cfg(test)]
mod grid_observation_tests {
    use super::*;

    fn tabs() -> String {
        use aterm_types::app_inspection::{InspectionEnvelope, InspectionSubject};
        format!(
            "{}\nwindow id=0 rows=24 cols=80\n",
            InspectionEnvelope {
                revision: 3,
                subject: InspectionSubject::Tabs,
            }
            .header_line()
        )
    }

    const AUDIT: &str = "inspection-source source=presented view=3 window=0 generation=1 geometry=9536910db77ae1d7 compiled-fingerprint=0123456789abcdef capture-serial=8 presentation-revision=9 model-current=true\n";

    #[test]
    fn qualification_grid_and_presented_identity_are_observed_strictly() {
        let reading = grid_observation(&tabs(), AUDIT).unwrap();
        assert_eq!((reading.rows, reading.cols), (24, 80));
        assert!(reading.presented && reading.current);
        assert_eq!(reading.capture, 8);
        for bad in [
            tabs().replace("cols=80", ""),
            tabs().replace("rows=24", "rows=0"),
            tabs().replace("rows=24", "rows=24 rows=25"),
            format!("{}window id=1 rows=24 cols=80\n", tabs()),
            "ERR unavailable".into(),
        ] {
            assert!(grid_observation(&bad, AUDIT).is_err());
        }
        for bad in [
            AUDIT.replace("geometry=9536910db77ae1d7", "geometry=invalid"),
            AUDIT.replace("capture-serial=8", ""),
            AUDIT.replace("view=3", ""),
            AUDIT.replace("window=0", "window=1"),
            AUDIT.replace("generation=1", ""),
            AUDIT.replace("compiled-fingerprint=0123456789abcdef", ""),
            AUDIT.replace("capture-serial=8", "capture-serial=8 capture-serial=9"),
            format!("{AUDIT}{AUDIT}"),
        ] {
            assert!(grid_observation(&tabs(), &bad).is_err());
        }
        let staged =
            grid_observation(&tabs(), &AUDIT.replace("source=presented", "source=staged")).unwrap();
        assert!(!staged.presented);
        let stale = grid_observation(
            &tabs(),
            &AUDIT.replace("model-current=true", "model-current=false"),
        )
        .unwrap();
        assert!(!stale.current);
    }

    #[test]
    fn explicit_grid_cases_are_ceiling_projections_not_pixel_acceptance() {
        assert_eq!(projected_grid(318, 516), Ok((22, 33)));
        assert_eq!(projected_grid(584, 348), Ok((15, 63)));
        assert_eq!(projected_grid(1095, 696), Ok((30, 119)));
        assert!(projected_grid(24, 516).is_err());
        assert!(projected_grid(318, 12).is_err());
    }
}

fn cursor_ready_to_press(text: &str, point: (i32, i32)) -> Result<bool, String> {
    let fields = measure::fields(text.trim());
    for (name, expected) in [
        ("left_down", "0"),
        ("target-owned", "1"),
        ("cursor-owned", "1"),
        ("foreground", "1"),
    ] {
        if one_field(&fields, name) != Some(expected) {
            return Err(format!(
                "actual cursor {name} is held, unknown or not owned; no native press"
            ));
        }
    }
    let actual = one_field(&fields, "cursor")
        .and_then(|value| value.split_once(','))
        .and_then(|(x, y)| Some((x.parse::<i32>().ok()?, y.parse::<i32>().ok()?)))
        .ok_or("actual cursor coordinates are unknown or malformed; no native press")?;
    Ok(actual == point)
}

fn actual_cursor_button(text: &str, point: (i32, i32), down: bool) -> bool {
    let fields = measure::fields(text.trim());
    let expected = format!("{},{}", point.0, point.1);
    one_field(&fields, "target-owned") == Some("1")
        && one_field(&fields, "left_down") == Some(if down { "1" } else { "0" })
        && one_field(&fields, "cursor") == Some(expected.as_str())
        && one_field(&fields, "cursor-owned") == Some("1")
        && one_field(&fields, "foreground") == Some("1")
}

fn monitor_contains(monitor: desk::Rect, window: desk::Rect) -> bool {
    window.width() > 0
        && window.height() > 0
        && window.l >= monitor.l
        && window.t >= monitor.t
        && window.r <= monitor.r
        && window.b <= monitor.b
}

#[cfg(test)]
mod actual_cursor_tests {
    use super::*;

    #[test]
    fn requested_point_is_not_actual_cursor_ownership() {
        let raw = "target-owned=1 left_down=0 cursor=100,200 cursor-owned=1 foreground=1";
        assert!(actual_cursor_button(raw, (100, 200), false));
        assert!(!actual_cursor_button(raw, (101, 200), false));
        assert!(!actual_cursor_button(raw, (100, 200), true));
        assert_eq!(cursor_ready_to_press(raw, (100, 200)), Ok(true));
        assert_eq!(cursor_ready_to_press(raw, (101, 200)), Ok(false));
        for bad in [
            raw.replace("left_down=0", "left_down=1"),
            raw.replace("left_down=0", "left_down=unknown"),
            raw.replace("target-owned=1", "target-owned=0"),
            raw.replace("cursor-owned=1", "cursor-owned=0"),
            raw.replace("foreground=1", "foreground=0"),
            raw.replace("cursor=100,200", "cursor=unknown"),
            raw.replace("cursor=100,200", "cursor=100,broken"),
            format!("{raw} left_down=0"),
        ] {
            assert!(cursor_ready_to_press(&bad, (100, 200)).is_err(), "{bad}");
        }
        for bad in [
            raw.replace("cursor=100,200", "cursor=unknown"),
            raw.replace("cursor-owned=1", "cursor-owned=0"),
            raw.replace("foreground=1", "foreground=0"),
            format!("{raw} cursor=100,200"),
            format!("{raw} left_down=1"),
            format!("{raw} target-owned=0"),
            "target-owned=1 left_down=0".into(),
        ] {
            assert!(!actual_cursor_button(&bad, (100, 200), false));
        }
        let monitor = desk::Rect {
            l: 800,
            t: 0,
            r: 2720,
            b: 1200,
        };
        assert!(monitor_contains(
            monitor,
            desk::Rect {
                l: 816,
                t: 16,
                r: 1950,
                b: 820
            }
        ));
        assert!(!monitor_contains(
            monitor,
            desk::Rect {
                l: 26,
                t: 26,
                r: 1160,
                b: 830
            }
        ));
        assert!(!monitor_contains(
            monitor,
            desk::Rect {
                l: 816,
                t: 16,
                r: 1950,
                b: 1220
            }
        ));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NativeMapping {
    view: u64,
    logical: (i32, i32),
    client: (i32, i32),
    origin: (i32, i32),
}

fn integer_coordinate(value: &str) -> Option<i32> {
    let value = value.parse::<f64>().ok()?;
    (value.is_finite()
        && value.fract() == 0.0
        && value >= f64::from(i32::MIN)
        && value <= f64::from(i32::MAX))
    .then_some(value as i32)
}

fn native_mapping(raw: &str, view: &str, key: &str, scale: f64) -> Result<NativeMapping, String> {
    if scale != 1.0 || !raw.trim().starts_with("OK ") || raw.trim().lines().count() != 1 {
        return Err("native mapping requires one scale-one application pointer status".into());
    }
    let fields = measure::fields(raw.trim());
    let parse = || -> Option<NativeMapping> {
        if one_field(&fields, "at")? != "native"
            || one_field(&fields, "view")? != view
            || one_field(&fields, "key")? != key
        {
            return None;
        }
        let view = view.parse::<u64>().ok().filter(|value| *value > 0)?;
        let logical = (
            integer_coordinate(one_field(&fields, "x")?)?,
            integer_coordinate(one_field(&fields, "y")?)?,
        );
        let (x, y) = one_field(&fields, "px")?.split_once(',')?;
        let client = (x.parse::<i32>().ok()?, y.parse::<i32>().ok()?);
        let origin = (
            client.0.checked_sub(logical.0)?,
            client.1.checked_sub(logical.1)?,
        );
        Some(NativeMapping {
            view,
            logical,
            client,
            origin,
        })
    };
    parse().ok_or_else(|| "application native mapping is missing, ambiguous or does not match the exact view/control".into())
}

impl NativeMapping {
    fn screen(self, client_origin: (i32, i32)) -> Result<(i32, i32), String> {
        client_origin
            .0
            .checked_add(self.client.0)
            .zip(client_origin.1.checked_add(self.client.1))
            .ok_or_else(|| "native screen coordinate overflow".into())
    }

    fn inverse(self, screen: (i32, i32), client_origin: (i32, i32)) -> Option<(i32, i32)> {
        Some((
            screen
                .0
                .checked_sub(client_origin.0)?
                .checked_sub(self.origin.0)?,
            screen
                .1
                .checked_sub(client_origin.1)?
                .checked_sub(self.origin.1)?,
        ))
    }
}

fn released_owned_target(raw: &str) -> bool {
    let fields = measure::fields(raw.trim());
    one_field(&fields, "left_down") == Some("0")
        && one_field(&fields, "target-owned") == Some("1")
        && one_field(&fields, "foreground") == Some("1")
        && one_field(&fields, "cursor")
            .and_then(|value| value.split_once(','))
            .is_some_and(|(x, y)| x.parse::<i32>().is_ok() && y.parse::<i32>().is_ok())
        && matches!(one_field(&fields, "cursor-owned"), Some("0" | "1"))
        && one_field(&fields, "owned-hwnd")
            .and_then(|value| u64::from_str_radix(value, 16).ok())
            .is_some_and(|value| value != 0)
}

#[cfg(test)]
mod native_mapping_tests {
    use super::*;

    #[test]
    fn returned_native_coordinates_supply_both_transform_directions() {
        let raw = "OK at=native view=3 x=425.0 y=126.0 key=settings/control/flag px=425,164";
        let mapped = native_mapping(raw, "3", "settings/control/flag", 1.0).unwrap();
        assert_eq!(mapped.origin, (0, 38));
        assert_eq!(mapped.screen((20, 40)), Ok((445, 204)));
        assert_eq!(mapped.inverse((445, 204), (20, 40)), Some((425, 126)));
        let other = native_mapping(
            &raw.replace("px=425,164", "px=437,193"),
            "3",
            "settings/control/flag",
            1.0,
        )
        .unwrap();
        assert_eq!(other.origin, (12, 67));
        assert_eq!(
            other.inverse(other.screen((7, 9)).unwrap(), (7, 9)),
            Some((425, 126))
        );
        for bad in [
            raw.replace("at=native", "at=terminal"),
            raw.replace("OK ", "ERR "),
            format!("{raw} at=native"),
            format!("{raw} key=settings/control/flag"),
            format!("{raw} px=425,164"),
            raw.replace("view=3", "view=4"),
            raw.replace("key=settings/control/flag", "key=settings/control/other"),
            raw.replace("x=425.0", "x=NaN"),
            raw.replace("x=425.0", "x=inf"),
            raw.replace("x=425.0", "x=2147483648.0"),
            raw.replace("x=425.0", "x=-2147483648.0")
                .replace("px=425,164", "px=2147483647,164"),
            raw.replace("y=126.0", "y=126.5"),
            raw.replace("px=425,164", "px=unknown"),
            raw.replace("px=425,164", ""),
            format!("{raw} view=3"),
            format!("{raw} x=425.0"),
            format!("{raw}\n{raw}"),
        ] {
            assert!(
                native_mapping(&bad, "3", "settings/control/flag", 1.0).is_err(),
                "{bad}"
            );
        }
        assert!(native_mapping(raw, "3", "settings/control/flag", 2.0).is_err());
        assert!(mapped.screen((i32::MAX, i32::MAX)).is_err());
        assert_eq!(
            mapped.inverse((i32::MIN, i32::MIN), (i32::MAX, i32::MAX)),
            None
        );
        assert!(
            native_mapping(
                &raw.replace("view=3", "view=0"),
                "0",
                "settings/control/flag",
                1.0
            )
            .is_err()
        );

        let up =
            "target-owned=1 foreground=1 left_down=0 owned-hwnd=ab12 cursor=10,20 cursor-owned=0";
        assert!(released_owned_target(up));
        for bad in [
            up.replace("left_down=0", "left_down=1"),
            up.replace("owned-hwnd=ab12", "owned-hwnd=0"),
            format!("{up} left_down=0"),
            up.replace("foreground=1", "foreground=0"),
        ] {
            assert!(!released_owned_target(&bad));
        }
    }
}
