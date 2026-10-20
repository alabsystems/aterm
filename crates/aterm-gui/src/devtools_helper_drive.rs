// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The Developer Tools helper panel's driver: the panel's AppKit views
//! (`devtools_helper::live`), built exactly as the live arm builds them, in NO
//! window, captured to PNGs and read back — the look the unit tests cannot give
//! (libtest is never on the main thread, and a test that needs WindowServer is a
//! driver, never a unit test).
//!
//! # What it refuses to do, which is why it may run beside the owner
//!
//! * Before it touches anything else it makes the application object with the
//!   PROHIBITED activation policy: no Dock icon, no menu bar, never activated.
//! * It never orders a window in and never makes one key. The panel window it
//!   reads back is built and released without being shown; every capture is
//!   `-cacheDisplayInRect:` of a content view that sits in no window.
//! * It reads no other app: not System Settings, not the window list, not which
//!   app is in front.
//! * It writes PNGs only into the directory it is handed (none without one),
//!   and exits on its own. The directory is the example's argument, never an
//!   environment variable: this file ships in the `aterm` binary, and shipped code
//!   reads no `ATERM_*` name a person could set.
//!
//! # What it checks
//!
//! 1. Every state of the panel's words builds, in the light and the dark
//!    appearance: the content view is [`PANEL_W`] wide, every view on it lies
//!    inside it, every label is as tall as its words need at its width (the
//!    layout measured them with the same labels), the tile is there exactly when
//!    the words carry one, and the capture is not one flat colour.
//! 2. The panel window reads back as built: its style mask, level, collection
//!    behaviour and flags, its title and delegate — and it is not visible.
//! 3. The drag's payload for a bundle path with a space and parentheses,
//!    written through AppKit's pasteboard machinery to a private, uniquely named
//!    pasteboard (never the owner's clipboard): the file URL percent-encoded with
//!    its trailing slash under both URL types and the plain path — and the old
//!    filenames list, which aterm does not write (AppKit refuses it as a written
//!    type) and AppKit derives from the file URL.
//! 4. The strings the panel spells for the window list's keys and for the
//!    activation notification equal the SDK's exported symbols (`dlsym`).
//! 5. The tile's NSDraggingSource rows and the target's `windowShouldClose:` are
//!    registered with the encodings their protocols declare.
//!
//! Its sends go through `crate::appkit`, as the panel's do: a debug build checks
//! each typed send against the runtime's encoding (`aterm_objc::send`), and this
//! reads what they built.
//!
//! # Exit contract (read by `examples/objc_devtools_helper_drive.rs`)
//!
//! | code | meaning |
//! |---|---|
//! | 0 | every check passed |
//! | 1 | at least one finding; the transcript names each |
//! | 2 | not run: not on the process main thread. Never a pass. |
//!
//! It is NOT an `aterm_verify::stages::OBJC_DRIVES` row: a look at the panel is
//! not a check every verify should pay for. Run it by hand:
//! `targo --unverified run -p aterm-gui --example objc_devtools_helper_drive --
//! <dir>`.

/// Every check passed.
const PASS: i32 = 0;
/// At least one finding. See the transcript.
const FINDING: i32 = 1;
/// The drive could not execute here. NOT a pass.
const NOT_RUN: i32 = 2;

/// Drive the panel's views and return the exit code above, with the PNGs under
/// `snapshots` when given. Called from `examples/objc_devtools_helper_drive.rs`,
/// which owns `fn main` and its argument; the body lives here because
/// `devtools_helper` is library-private.
#[must_use]
pub fn run_devtools_helper_drive(snapshots: Option<&Path>) -> i32 {
    // The drive is a windowed process of its own: it makes the application
    // object and builds AppKit views, so it mints the proof a windowed launch
    // carries and hands it to the body that sends `+sharedApplication`.
    run(crate::platform::Windowed::launch(), snapshots)
}

use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aterm_containment::DevToolGrant;
use aterm_containment::consent::{
    DevToolList, DevToolListLabel, DevToolListing, DevToolProbe, DevToolProbeLabel,
};
use aterm_objc::{CGPoint, CGRect, CGSize, ClassType as _, Id, Obj, class, sel};

use crate::appkit::{self, MainThread, consts};
use crate::devtools_helper::live::{self, Preview};
use crate::devtools_helper::{
    Block, Content, DROP_CONFIRM_WITHIN, Origin, PANEL_W, Page, Panel, PanelEvent, Reading, Show,
};

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `RTLD_DEFAULT`, as dlfcn.h defines it on Darwin.
const RTLD_DEFAULT: *mut c_void = (-2_isize) as *mut c_void;

/// The bundle the previews' tiles show.
const TILE_BUNDLE: &str = "/Applications/aterm.app";

/// A bundle path a hand-formatted URL gets wrong (`targo --unverified ship dev-app` installs it).
const AWKWARD_BUNDLE: &str = "/Applications/aterm (dev).app";

/// The transcript's prefix.
const ME: &str = "objc-devtools-helper-drive";

/// The findings so far.
#[derive(Default)]
struct Findings(Vec<String>);

impl Findings {
    fn check(&mut self, ok: bool, what: impl FnOnce() -> String) {
        if !ok {
            let what = what();
            println!("{ME}: FINDING {what}");
            self.0.push(what);
        }
    }
}

/// The body: the one function here that makes the application object, holding the
/// windowed launch's proof (aterm-census's `shared-application` rule), and only with
/// the Prohibited policy.
fn run(_windowed: crate::platform::Windowed, out: Option<&Path>) -> i32 {
    let Some(mtm) = MainThread::new() else {
        println!("{ME}: NOT RUN — not on the process main thread");
        return NOT_RUN;
    };
    // (a) The application object, Prohibited, before anything else: never
    // activated, no Dock icon, no menu bar.
    // SAFETY: main thread; `+sharedApplication` is `+(NSApplication *)`, +0 and
    // process-lifetime; `-setActivationPolicy:` is `-(BOOL)(NSInteger)` and
    // `-activationPolicy` `-(NSInteger)`.
    let policy = unsafe {
        let app = appkit::send_id(class(c"NSApplication").as_id(), sel!(sharedApplication));
        if app.is_null() {
            println!("{ME}: NOT RUN — no application object");
            return NOT_RUN;
        }
        let _ = appkit::send_bool_isize(
            app,
            sel!(setActivationPolicy:),
            consts::NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED,
        );
        appkit::send_isize(app, sel!(activationPolicy))
    };
    if policy != consts::NS_APPLICATION_ACTIVATION_POLICY_PROHIBITED {
        println!("{ME}: NOT RUN — the Prohibited activation policy did not take ({policy})");
        return NOT_RUN;
    }
    let mut findings = Findings::default();
    aterm_objc::autoreleasepool(|_| {
        states(mtm, out, &mut findings);
        panel_window(mtm, &mut findings);
        payload(&mut findings);
        symbols(&mut findings);
        encodings(&mut findings);
    });
    if findings.0.is_empty() {
        println!("{ME}: PASS");
        PASS
    } else {
        println!("{ME}: {} finding(s)", findings.0.len());
        FINDING
    }
}

/// A list row.
fn list(listing: DevToolListing, label: DevToolListLabel, rows: Option<u32>) -> DevToolList {
    DevToolList {
        listing,
        label,
        modified_at_s: None,
        rows,
    }
}

/// A panel shown for `list`, after a `spctl` run that answered `listed`.
fn shown_after(
    list: DevToolList,
    bundle: bool,
    listed: Option<Result<(), String>>,
    at: Instant,
) -> Panel {
    Panel::new(
        Show {
            page: Page::DeveloperTools,
            origin: Origin::Digest,
            reading: Reading {
                grant: DevToolGrant::NotGranted,
                list,
                log: DevToolProbe {
                    requests: Some(3),
                    not_allowed: Some(3),
                    ..DevToolProbe::refused(DevToolProbeLabel::Observed)
                },
                subject: Some("com.aterm.aterm"),
            },
            listed,
            bundle: bundle.then(|| PathBuf::from(TILE_BUNDLE)),
            skips: aterm_containment::SpikeEvidence::MEASURED.devtool_skips_first_run_check,
        },
        at,
    )
}

/// A panel shown for `list`; `listed_terminal` when this hand-over's `spctl` run
/// answered Ok.
fn shown(list: DevToolList, bundle: bool, listed_terminal: bool, at: Instant) -> Panel {
    shown_after(list, bundle, listed_terminal.then_some(Ok(())), at)
}

/// Every state the panel's words take, by name.
fn scenarios() -> Vec<(&'static str, Content)> {
    let t0 = Instant::now();
    let absent = list(DevToolListing::Absent, DevToolListLabel::Read, Some(2));
    let mut out = vec![
        ("explain", shown(absent, true, false, t0).content()),
        (
            "explain-terminal-added",
            shown(
                list(DevToolListing::Absent, DevToolListLabel::Read, Some(0)),
                true,
                true,
                t0,
            )
            .content(),
        ),
        (
            // The hand-over's `spctl` run failed on an empty list: why, the +
            // route, and Show it.
            "list-not-shown",
            shown_after(
                list(DevToolListing::Absent, DevToolListLabel::Read, Some(0)),
                true,
                Some(Err("spctl exited 1".to_string())),
                t0,
            )
            .content(),
        ),
        ("words-only", shown(absent, false, false, t0).content()),
        (
            "listed-off",
            shown(
                list(DevToolListing::Off, DevToolListLabel::Read, Some(2)),
                true,
                false,
                t0,
            )
            .content(),
        ),
        (
            // On the list and switched on while macOS still checks (the joined
            // verdict not granted): nothing to drag, nothing to switch on.
            "listed-on",
            shown(
                list(DevToolListing::On, DevToolListLabel::Read, Some(2)),
                true,
                false,
                t0,
            )
            .content(),
        ),
        (
            "unread-full-disk-access",
            shown(
                DevToolList::refused(DevToolListLabel::NoFullDiskAccess),
                true,
                false,
                t0,
            )
            .content(),
        ),
        (
            "unread-probe-off",
            shown(
                DevToolList::refused(DevToolListLabel::RefusedDisabled),
                true,
                false,
                t0,
            )
            .content(),
        ),
    ];
    let mut failed = shown(absent, true, false, t0);
    let _ = failed.on(PanelEvent::DragEnded { accepted: false }, t0);
    out.push(("drop-not-taken", failed.content()));
    let mut outside = shown(absent, true, false, t0);
    let _ = outside.on(PanelEvent::DroppedOutside, t0);
    out.push(("drop-outside", outside.content()));
    let mut unconfirmed = shown(absent, true, false, t0);
    let _ = unconfirmed.on(PanelEvent::DragEnded { accepted: true }, t0);
    let _ = unconfirmed.on_read(absent, t0 + DROP_CONFIRM_WITHIN + Duration::from_secs(1));
    out.push(("drop-unconfirmed", unconfirmed.content()));
    let mut done = shown(absent, true, false, t0);
    let _ = done.on_read(
        list(DevToolListing::On, DevToolListLabel::Read, Some(2)),
        t0,
    );
    out.push(("done", done.content()));
    // The Full Disk Access page's panel (owner ask 2026-10-16): no list read, so no
    // listed, unread or unconfirmed states.
    let fda = |bundle: bool| {
        Panel::new(
            Show {
                page: Page::FullDiskAccess,
                origin: Origin::Automatic,
                reading: Reading::UNREAD,
                listed: None,
                bundle: bundle.then(|| PathBuf::from(TILE_BUNDLE)),
                skips: false,
            },
            t0,
        )
    };
    out.push(("fda-explain", fda(true).content()));
    out.push(("fda-words-only", fda(false).content()));
    let mut fda_failed = fda(true);
    let _ = fda_failed.on(PanelEvent::DragEnded { accepted: false }, t0);
    out.push(("fda-drop-not-taken", fda_failed.content()));
    let mut fda_dropped = fda(true);
    let _ = fda_dropped.on(PanelEvent::DragEnded { accepted: true }, t0);
    out.push(("fda-dropped", fda_dropped.content()));
    let mut fda_done = fda(true);
    let _ = fda_done.granted(t0);
    out.push(("fda-done", fda_done.content()));
    out
}

/// Check 1: every state, light and dark.
fn states(mtm: MainThread, out: Option<&Path>, findings: &mut Findings) {
    for (name, content) in scenarios() {
        let wants_tile = content
            .blocks
            .iter()
            .any(|block| matches!(block, Block::Tile(_)));
        for dark in [false, true] {
            let look = if dark { "dark" } else { "light" };
            let at = format!("{name} ({look})");
            let Some(preview) = live::preview(mtm, &content, false) else {
                findings.check(false, || format!("{at}: the views did not build"));
                continue;
            };
            // SAFETY: main thread; the preview's views are live.
            unsafe { view_checks(&preview, content.blocks.len(), wants_tile, &at, findings) };
            // SAFETY: as above.
            match unsafe { capture(preview.view.as_id(), dark) } {
                Some((w, h, png, flat)) => {
                    findings.check(!flat, || format!("{at}: the capture is one flat colour"));
                    println!("{ME}: {at}: {w}x{h} px");
                    if let Some(dir) = out {
                        let file = dir.join(format!("devtools-helper-{name}-{look}.png"));
                        match std::fs::write(&file, &png) {
                            Ok(()) => println!("{ME}: wrote {}", file.display()),
                            Err(e) => {
                                findings.check(false, || format!("{at}: {}: {e}", file.display()))
                            }
                        }
                    }
                }
                None => findings.check(false, || format!("{at}: nothing was captured")),
            }
        }
    }
}

/// The content view's shape: its width, at least one view per block, its views
/// inside it, each label tall enough for its words, the tile present exactly
/// when wanted.
///
/// # Safety
/// Main thread; `preview`'s views are live.
unsafe fn view_checks(
    preview: &Preview,
    blocks: usize,
    wants_tile: bool,
    at: &str,
    findings: &mut Findings,
) {
    // SAFETY: `-bounds`/`-frame` are `-(NSRect)`; `-subviews` `-(NSArray *)`,
    // +0, read with `-count`/`-objectAtIndex:`; `-isKindOfClass:` is
    // `-(BOOL)(Class)`; `-cell` `-(id)` and `-cellSizeForBounds:`
    // `-(NSSize)(NSRect)`.
    unsafe {
        let view = preview.view.as_id();
        let bounds = appkit::send_rect(view, sel!(bounds));
        findings.check((bounds.size.width - PANEL_W).abs() < 0.5, || {
            format!(
                "{at}: the content view is {} wide, not {PANEL_W}",
                bounds.size.width
            )
        });
        let subviews = appkit::send_id(view, sel!(subviews));
        let n = if subviews.is_null() {
            0
        } else {
            appkit::send_usize(subviews, sel!(count))
        };
        findings.check(n >= blocks, || {
            format!("{at}: {n} views for {blocks} blocks")
        });
        let label = class(c"NSTextField");
        for i in 0..n {
            let sub = appkit::send_id_usize(subviews, sel!(objectAtIndex:), i);
            let f = appkit::send_rect(sub, sel!(frame));
            let inside = f.origin.x >= -0.5
                && f.origin.y >= -0.5
                && f.origin.x + f.size.width <= bounds.size.width + 0.5
                && f.origin.y + f.size.height <= bounds.size.height + 0.5;
            findings.check(inside, || {
                format!("{at}: view {i} at {f:?} leaves {bounds:?}")
            });
            if appkit::send_bool_cls(sub, sel!(isKindOfClass:), label) {
                let cell = appkit::send_id(sub, sel!(cell));
                let need = appkit::send_size_rect(
                    cell,
                    sel!(cellSizeForBounds:),
                    CGRect {
                        origin: CGPoint::default(),
                        size: CGSize {
                            width: f.size.width,
                            height: f64::from(u16::MAX),
                        },
                    },
                );
                findings.check(need.height <= f.size.height + 0.5, || {
                    format!(
                        "{at}: label {i} needs {} pt and has {}",
                        need.height, f.size.height
                    )
                });
            }
        }
        findings.check(preview.tile.is_some() == wants_tile, || {
            format!(
                "{at}: tile {} but the words {} one",
                preview.tile.is_some(),
                if wants_tile { "carry" } else { "carry no" }
            )
        });
    }
}

/// `view` in the light or dark appearance, through AppKit's caching pair, as a
/// PNG: `(width px, height px, png, flat)` — `flat` when every sampled pixel is
/// the first one's colour.
///
/// # Safety
/// Main thread; `view` is a live NSView in no window.
unsafe fn capture(view: Id, dark: bool) -> Option<(usize, usize, Vec<u8>, bool)> {
    // SAFETY: `+appearanceNamed:` is `+(NSAppearance *)(NSString *)`, +0, given
    // AppKit's own name global; `-setAppearance:` `-(void)(id)`; the caching
    // pair `-(NSBitmapImageRep *)(NSRect)` and `-(void)(NSRect, id)`; the rep's
    // accessors `-(NSInteger)` and `-(unsigned char *)`;
    // `-representationUsingType:properties:` `-(NSData *)(NSUInteger, id)`;
    // `-length` `-(NSUInteger)` and `-getBytes:length:` `-(void)(void *,
    // NSUInteger)` into a buffer of exactly that length.
    unsafe {
        let look = appkit::send_id_id(
            class(c"NSAppearance").as_id(),
            sel!(appearanceNamed:),
            appkit::appearance_name(dark),
        );
        appkit::send_v_id(view, sel!(setAppearance:), look);
        let bounds = appkit::send_rect(view, sel!(bounds));
        let rep = appkit::send_id_rect(view, sel!(bitmapImageRepForCachingDisplayInRect:), bounds);
        if rep.is_null() {
            return None;
        }
        appkit::send_v_rect_id(
            view,
            sel!(cacheDisplayInRect:toBitmapImageRep:),
            bounds,
            rep,
        );
        let w = usize::try_from(appkit::send_isize(rep, sel!(pixelsWide))).unwrap_or(0);
        let h = usize::try_from(appkit::send_isize(rep, sel!(pixelsHigh))).unwrap_or(0);
        let stride = usize::try_from(appkit::send_isize(rep, sel!(bytesPerRow))).unwrap_or(0);
        let spp = usize::try_from(appkit::send_isize(rep, sel!(samplesPerPixel))).unwrap_or(0);
        let data = appkit::send_charptr(rep, sel!(bitmapData));
        if data.is_null() || w == 0 || h == 0 || stride == 0 || spp == 0 {
            return None;
        }
        let pixel =
            |x: usize, y: usize| std::slice::from_raw_parts(data.add(y * stride + x * spp), spp);
        let first = pixel(0, 0).to_vec();
        let flat = (0..h)
            .step_by(2)
            .all(|y| (0..w).step_by(2).all(|x| pixel(x, y) == first.as_slice()));
        let props = Obj::from_owned(appkit::send_id(class(c"NSDictionary").as_id(), sel!(new)))?;
        let png = appkit::send_id_usize_id(
            rep,
            sel!(representationUsingType:properties:),
            consts::NS_BITMAP_IMAGE_FILE_TYPE_PNG,
            props.id(),
        );
        if png.is_null() {
            return None;
        }
        let len = appkit::send_usize(png, sel!(length));
        let mut bytes = vec![0_u8; len];
        if len != 0 {
            appkit::send_v_ptr_usize(png, sel!(getBytes:length:), bytes.as_mut_ptr().cast(), len);
        }
        Some((w, h, bytes, flat))
    }
}

/// Check 2: the panel window reads back as built, and is not shown.
fn panel_window(mtm: MainThread, findings: &mut Findings) {
    let content = scenarios().swap_remove(0).1;
    let Some(preview) = live::preview(mtm, &content, true) else {
        findings.check(false, || "the panel window did not build".to_string());
        return;
    };
    let Some(panel) = preview.panel.as_ref() else {
        findings.check(false, || "the preview has no panel window".to_string());
        return;
    };
    let p = panel.id();
    // SAFETY: main thread; read-only `-(NSUInteger)`, `-(NSInteger)`, `-(BOOL)`
    // and `-(id)` accessors on the live, never-shown panel; then `-setDelegate:`
    // nil before it goes.
    unsafe {
        let mask = appkit::send_usize(p, sel!(styleMask));
        findings.check(mask == consts::NS_WINDOW_STYLE_MASK_UTILITY_PANEL, || {
            format!(
                "style mask {mask:#x}, not {:#x}",
                consts::NS_WINDOW_STYLE_MASK_UTILITY_PANEL
            )
        });
        let level = appkit::send_isize(p, sel!(level));
        findings.check(level == consts::NS_FLOATING_WINDOW_LEVEL, || {
            format!("level {level}")
        });
        let behavior = appkit::send_usize(p, sel!(collectionBehavior));
        findings.check(
            behavior == consts::NS_WINDOW_COLLECTION_BEHAVIOR_ALL_SPACES_AUXILIARY,
            || format!("collection behaviour {behavior:#x}"),
        );
        for (getter, want) in [
            (sel!(isFloatingPanel), true),
            (sel!(hidesOnDeactivate), false),
            (sel!(canHide), false),
            (sel!(becomesKeyOnlyIfNeeded), true),
            (sel!(isMovableByWindowBackground), false),
            (sel!(isReleasedWhenClosed), false),
            (sel!(isVisible), false),
        ] {
            let got = appkit::send_bool(p, getter);
            findings.check(got == want, || {
                format!("a panel flag reads {got}, not {want}")
            });
        }
        let title = appkit::nsstring_to_rust(appkit::send_id(p, sel!(title)));
        findings.check(title == "aterm", || format!("title {title:?}"));
        // The window holds the whole layout: its content rect is the size the same
        // words lay out to in no window. `-setContentView:` fits the view to the
        // window's first square, so a panel sized from the view's bounds kept that
        // square and cut off the buttons under taller words (measured live
        // 2026-10-16: a 384-point frame around 540 points of words).
        match live::preview(mtm, &content, false) {
            Some(bare) => {
                let laid = appkit::send_rect(bare.view.as_id(), sel!(frame));
                let frame = appkit::send_rect(p, sel!(frame));
                let inside = appkit::send_rect_rect(p, sel!(contentRectForFrameRect:), frame);
                findings.check(
                    (inside.size.width - laid.size.width).abs() < 0.5
                        && (inside.size.height - laid.size.height).abs() < 0.5,
                    || {
                        format!(
                            "the panel holds {}x{} points of a {}x{} layout",
                            inside.size.width,
                            inside.size.height,
                            laid.size.width,
                            laid.size.height
                        )
                    },
                );
            }
            None => findings.check(false, || "the bare views did not build".to_string()),
        }
        let delegate = appkit::send_id(p, sel!(delegate));
        findings.check(delegate == preview.target.as_id(), || {
            "the panel's delegate is not its target".to_string()
        });
        // The tile lights up from one hover area active always (cursor rects
        // work only in the key window, which the panel never is).
        match preview.tile.as_ref() {
            Some(tile) => {
                let areas = appkit::send_id(tile.as_id(), sel!(trackingAreas));
                let count = if areas.is_null() {
                    0
                } else {
                    appkit::send_usize(areas, sel!(count))
                };
                let options = if count == 1 {
                    let area = appkit::send_id_usize(areas, sel!(objectAtIndex:), 0);
                    appkit::send_usize(area, sel!(options))
                } else {
                    0
                };
                findings.check(
                    count == 1 && options == consts::NS_TRACKING_HOVER_IN_VISIBLE_RECT,
                    || format!("the tile has {count} tracking area(s), options {options:#x}"),
                );
                // What that area drives: the pointer coming over the live tile
                // lights it, and leaving puts it out.
                appkit::send_v_id(tile.as_id(), sel!(mouseEntered:), Id::NIL);
                let entered = tile.hovered();
                appkit::send_v_id(tile.as_id(), sel!(mouseExited:), Id::NIL);
                let exited = tile.hovered();
                findings.check(entered && !exited, || {
                    format!("the tile reads lit={entered} over it and lit={exited} after")
                });
            }
            None => findings.check(false, || "the explain panel built no tile".to_string()),
        }
        appkit::send_v_id(p, sel!(setDelegate:), Id::NIL);
    }
    println!("{ME}: the panel window reads back as built, not shown");
}

/// Check 3: the drag's payload, for a path a hand-built URL would get wrong —
/// written through AppKit's own pasteboard machinery (as a drag writes it) to a
/// private, uniquely named pasteboard, never the owner's clipboard, and read
/// back type by type.
fn payload(findings: &mut Findings) {
    // SAFETY: main thread; the item is built as the drag builds it.
    let Some(item) = (unsafe { live::pasteboard_item(Path::new(AWKWARD_BUNDLE)) }) else {
        findings.check(false, || "no drag payload was built".to_string());
        return;
    };
    // SAFETY: main thread. `+pasteboardWithUniqueName` is `+(NSPasteboard *)`,
    // +0, a pasteboard of this process's own; `-clearContents` is
    // `-(NSInteger)`; `-writeObjects:` `-(BOOL)(NSArray *)`; `-stringForType:`/
    // `-propertyListForType:` `-(id)(NSPasteboardType)`, +0; `-releaseGlobally`
    // `-(void)` gives the pasteboard back.
    unsafe {
        let board = appkit::send_id(
            class(c"NSPasteboard").as_id(),
            sel!(pasteboardWithUniqueName),
        );
        if board.is_null() {
            findings.check(false, || {
                "no private pasteboard to write the payload to".to_string()
            });
            return;
        }
        let _ = appkit::send_isize(board, sel!(clearContents));
        let objects =
            appkit::send_id_id(class(c"NSArray").as_id(), sel!(arrayWithObject:), item.id());
        let wrote = !objects.is_null() && appkit::send_bool_id(board, sel!(writeObjects:), objects);
        findings.check(wrote, || "the pasteboard refused the payload".to_string());
        let string = |ty: &str| {
            appkit::nsstring(ty).map(|ty| {
                appkit::nsstring_to_rust(appkit::send_id_id(board, sel!(stringForType:), ty.id()))
            })
        };
        let url = "file:///Applications/aterm%20(dev).app/";
        for ty in [live::TYPE_FILE_URL, live::TYPE_URL] {
            let got = string(ty);
            findings.check(got.as_deref() == Some(url), || {
                format!("{ty}: {got:?}, not {url:?}")
            });
        }
        let got = string(live::TYPE_TEXT);
        findings.check(got.as_deref() == Some(AWKWARD_BUNDLE), || {
            format!("{}: {got:?}", live::TYPE_TEXT)
        });
        let names = appkit::nsstring(live::TYPE_FILENAMES)
            .map(|ty| appkit::send_id_id(board, sel!(propertyListForType:), ty.id()))
            .unwrap_or(Id::NIL);
        let listed = !names.is_null()
            && appkit::send_usize(names, sel!(count)) == 1
            && appkit::nsstring_to_rust(appkit::send_id_usize(names, sel!(objectAtIndex:), 0))
                == AWKWARD_BUNDLE;
        findings.check(listed, || {
            format!("{}: not [{AWKWARD_BUNDLE:?}]", live::TYPE_FILENAMES)
        });
        appkit::send_v(board, sel!(releaseGlobally));
    }
    println!("{ME}: the drag payload is Finder's shape for {AWKWARD_BUNDLE:?}");
}

/// Check 4: the strings the panel spells are the SDK's symbols' values.
fn symbols(findings: &mut Findings) {
    for name in live::WINDOW_KEYS.iter().chain(live::WORKSPACE_NAMES.iter()) {
        let Ok(symbol) = std::ffi::CString::new(*name) else {
            continue;
        };
        // SAFETY: `dlsym` with `RTLD_DEFAULT` and a C string; a non-null answer
        // is the address of an exported `CFStringRef`/`NSString *` constant,
        // read once and only as a string.
        let value = unsafe {
            let at = dlsym(RTLD_DEFAULT, symbol.as_ptr()).cast::<Id>();
            if at.is_null() {
                None
            } else {
                Some(appkit::nsstring_to_rust(*at))
            }
        };
        findings.check(value.as_deref() == Some(*name), || {
            format!("{name}: the SDK's symbol holds {value:?}")
        });
    }
    println!("{ME}: the window-list keys and the activation names are the SDK's");
}

/// Check 5: the declared rows carry their protocols' encodings.
fn encodings(findings: &mut Findings) {
    let rows: [(&CStr, aterm_objc::ClassPtr, aterm_objc::Sel); 5] = [
        (
            c"NSDraggingSource",
            live::TileView::class(),
            sel!(draggingSession:sourceOperationMaskForDraggingContext:),
        ),
        (
            c"NSDraggingSource",
            live::TileView::class(),
            sel!(ignoreModifierKeysForDraggingSession:),
        ),
        (
            c"NSDraggingSource",
            live::TileView::class(),
            sel!(draggingSession:willBeginAtPoint:),
        ),
        (
            c"NSDraggingSource",
            live::TileView::class(),
            sel!(draggingSession:endedAtPoint:operation:),
        ),
        (
            c"NSWindowDelegate",
            live::HelperTarget::class(),
            sel!(windowShouldClose:),
        ),
    ];
    for (proto, cls, name) in rows {
        // SAFETY: `cls` is a registered class and `name` an interned selector;
        // the protocol is looked up, never registered here.
        let (got, want) = unsafe {
            (
                aterm_objc::method_types(cls, name).map(|t| aterm_objc::strip_method_offsets(&t)),
                aterm_objc::protocol_method_types(aterm_objc::protocol(proto), name, true)
                    .map(|t| aterm_objc::strip_method_offsets(&t)),
            )
        };
        findings.check(got.is_some() && got == want, || {
            format!("{proto:?} row registered as {got:?}, declared {want:?}")
        });
    }
    println!("{ME}: the tile's and the target's rows carry their protocols' encodings");
}
