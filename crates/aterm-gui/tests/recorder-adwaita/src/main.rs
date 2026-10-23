// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Oracle recorder for `sctk-adwaita` 0.10.1 button layouts, shadow profiles, and border planes.

#![allow(dead_code, unused_imports, mismatched_lifetime_syntaxes, clippy::all)]

mod buttons {
    include!(concat!(env!("OUT_DIR"), "/buttons.rs"));
}
mod config {
    include!(concat!(env!("OUT_DIR"), "/config.rs"));
}
mod theme {
    include!(concat!(env!("OUT_DIR"), "/theme.rs"));
}
mod shadow {
    include!(concat!(env!("OUT_DIR"), "/shadow.rs"));
}

/// `parts.rs` part indices.
mod parts {
    pub struct DecorationParts;
    impl DecorationParts {
        pub const TOP: usize = 0;
        pub const LEFT: usize = 1;
        pub const RIGHT: usize = 2;
        pub const BOTTOM: usize = 3;
        pub const HEADER: usize = 4;
    }
}

/// Stub `wayland-csd-frame` flag types referenced by `buttons.rs`.
mod smithay_client_toolkit {
    pub mod reexports {
        pub mod csd_frame {
            #[derive(Clone, Copy, Debug, PartialEq, Eq)]
            pub struct WindowState(u16);
            impl WindowState {
                pub const MAXIMIZED: Self = Self(0b0000_0000_0000_0001);
                pub fn contains(&self, other: Self) -> bool {
                    self.0 & other.0 == other.0
                }
            }
            #[derive(Clone, Copy, Debug, PartialEq, Eq)]
            pub struct WindowManagerCapabilities(u16);
            impl WindowManagerCapabilities {
                pub const MAXIMIZE: Self = Self(0b0000_0000_0000_0010);
                pub const MINIMIZE: Self = Self(0b0000_0000_0000_1000);
                pub fn contains(&self, other: Self) -> bool {
                    self.0 & other.0 == other.0
                }
            }
        }
    }
}

/// Pointer location within the client-side decoration frame.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum Location {
    #[default]
    None,
    Head,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    TopLeft,
    Button(buttons::ButtonKind),
}

type SkiaResult = Option<()>;

use std::path::PathBuf;

/// The header width the layout is read across, wide enough that no button
/// of one side overlaps the other's.
const HEADER_W: u32 = 800;

const VALUES: &[&str] = &[
    "appmenu:close",
    "appmenu:minimize,maximize,close",
    "close,minimize,maximize:",
    "close:",
    ":close",
    "minimize,maximize,close:appmenu",
    "close,maximize,minimize:menu",
    "icon:minimize,maximize,close",
    "menu:minimize, maximize ,close",
    "minimize,maximize,close",
    ":",
    "",
    "appmenu:",
    "appmenu:spacer",
    "close:close",
    "maximize:minimize",
    "minimize:maximize:close",
    "appmenu,close,minimize,maximize:",
    ":appmenu,close,minimize,maximize",
    ":close,close,close,close",
    "Close:Minimize",
    "close,minimize,maximize:minimize,maximize,close",
];

fn kind_name(k: buttons::ButtonKind) -> &'static str {
    match k {
        buttons::ButtonKind::Close => "close",
        buttons::ButtonKind::Maximize => "maximize",
        buttons::ButtonKind::Minimize => "minimize",
    }
}

fn read_config(value: Option<&str>, fake_dir: &std::path::Path) -> Option<(String, String)> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![fake_dir.to_path_buf()];
    dirs.extend(std::env::split_paths(&path));
    std::env::set_var("PATH", std::env::join_paths(dirs).expect("PATH"));
    match value {
        Some(v) => std::env::set_var("RECORDER_BUTTON_LAYOUT", v),
        None => std::env::remove_var("RECORDER_BUTTON_LAYOUT"),
    }
    let read = config::get_button_layout_config();
    std::env::set_var("PATH", path);
    read
}

fn join(v: &[&str]) -> String {
    if v.is_empty() {
        "-".into()
    } else {
        v.join(",")
    }
}

const PLANE_SIZES: &[(u32, u32)] = &[(744, 528), (400, 300), (1280, 800), (401, 299), (30, 20)];

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// Prints FNV-1a and alpha sums for each border part across [`PLANE_SIZES`].
fn planes() {
    let depth = theme::BORDER_SIZE;
    for &(w, h) in PLANE_SIZES {
        for (name, idx) in [
            ("top", parts::DecorationParts::TOP),
            ("left", parts::DecorationParts::LEFT),
            ("right", parts::DecorationParts::RIGHT),
            ("bottom", parts::DecorationParts::BOTTOM),
        ] {
            for scale in [1u32, 2, 3] {
                for active in [true, false] {
                    let (pw, ph) = match idx {
                        parts::DecorationParts::TOP | parts::DecorationParts::BOTTOM => {
                            (w + 2 * depth, depth)
                        }
                        _ => (depth, h),
                    };
                    let (pw, ph) = (pw * scale, ph * scale);
                    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).expect("a part's pixmap");
                    let mut shadow = shadow::Shadow::default();
                    shadow.draw(&mut pixmap.as_mut(), scale, active, idx);
                    let alpha: u64 = pixmap.pixels().iter().map(|p| u64::from(p.alpha())).sum();
                    println!(
                        "plane\tpart={name}\twindow={w}x{h}\tscale={scale}\tactive={}\tbuffer={pw}x{ph}\talpha_sum={alpha}\tfnv={:016x}",
                        u8::from(active),
                        fnv(pixmap.data()),
                    );
                }
            }
        }
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("planes") {
        planes();
        return;
    }
    let fake_dir: PathBuf = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("the binaries' directory")
        .to_path_buf();
    assert!(
        fake_dir.join("dbus-send").is_file(),
        "build the dbus-send stand-in first (`targo --unverified build --release --locked`)"
    );
    let mut values: Vec<Option<&str>> = vec![None];
    values.extend(VALUES.iter().copied().map(Some));
    for value in values {
        let read = read_config(value, &fake_dir);
        let read_s = match &read {
            Some((l, r)) => format!("{l}|{r}"),
            None => "-".into(),
        };
        for (max, min) in [(true, true), (true, false), (false, true), (false, false)] {
            let mut b = buttons::Buttons::new(read.clone());
            b.update_buttons(max, min);
            b.arrange(HEADER_W, 0.0);
            let (left, right) = sides(&b);
            println!(
                "layout\tvalue={}\tread={read_s}\tmaximize={}\tminimize={}\tleft={}\tright={}",
                value.map_or("<no portal>".to_string(), |v| format!("{v:?}")),
                u8::from(max),
                u8::from(min),
                join(&left),
                join(&right),
            );
        }
    }
    for part in [
        ("top", parts::DecorationParts::TOP),
        ("left", parts::DecorationParts::LEFT),
        ("right", parts::DecorationParts::RIGHT),
        ("bottom", parts::DecorationParts::BOTTOM),
    ] {
        for scale in [1u32, 2] {
            for active in [true, false] {
                println!(
                    "shadow\tpart={}\tscale={scale}\tactive={}\talpha={}",
                    part.0,
                    u8::from(active),
                    profile(part.1, scale, active)
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                );
            }
        }
    }
    println!(
        "size\tshadow={}\tvisible_border={}\tborder={}\theader={}\tcorner_radius={}\tresize_handle={}\tresize_handle_corner={}",
        shadow::SHADOW_SIZE,
        theme::VISIBLE_BORDER_SIZE,
        theme::BORDER_SIZE,
        theme::HEADER_SIZE,
        theme::CORNER_RADIUS,
        theme::RESIZE_HANDLE_SIZE,
        theme::RESIZE_HANDLE_CORNER_SIZE,
    );
}

/// Returns `(left, right)` button names across the header button row.
fn sides(b: &buttons::Buttons) -> (Vec<&'static str>, Vec<&'static str>) {
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let mut last = Location::None;
    // The row's middle (BUTTON_MARGIN 5 + half of BUTTON_SIZE 24).
    let y = 17.0;
    for x in 0..HEADER_W {
        let at = b.find_button(f64::from(x) + 0.5, y);
        if at != last {
            if let Location::Button(k) = at {
                if x < HEADER_W / 2 {
                    left.push(kind_name(k));
                } else {
                    right.push(kind_name(k));
                }
            }
        }
        last = at;
    }
    (left, right)
}

/// Samples shadow alpha outward along the middle line of part `idx`.
fn profile(idx: usize, scale: u32, active: bool) -> Vec<u8> {
    let (w, h) = (400u32, 300u32);
    let depth = theme::BORDER_SIZE;
    let (pw, ph) = match idx {
        parts::DecorationParts::TOP | parts::DecorationParts::BOTTOM => (w + 2 * depth, depth),
        _ => (depth, h + theme::HEADER_SIZE),
    };
    let (pw, ph) = (pw * scale, ph * scale);
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).expect("a part's pixmap");
    let mut shadow = shadow::Shadow::default();
    shadow.draw(&mut pixmap.as_mut(), scale, active, idx);
    let at = |x: u32, y: u32| pixmap.pixels()[(y * pw + x) as usize].alpha();
    let d = depth * scale;
    match idx {
        // The top part's bottom row meets the header.
        parts::DecorationParts::TOP => (0..d).map(|i| at(pw / 2, ph - 1 - i)).collect(),
        // The left part's right column meets the window.
        parts::DecorationParts::LEFT => (0..d).map(|i| at(pw - 1 - i, ph / 2)).collect(),
        parts::DecorationParts::RIGHT => (0..d).map(|i| at(i, ph / 2)).collect(),
        _ => (0..d).map(|i| at(pw / 2, i)).collect(),
    }
}
