// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The BUNDLED TERMINAL FONT STACK (non-macOS).
//!
//! Owner decision (2026-09-24): the fonts a stock Linux ships are poor and
//! incomplete — `fc-match monospace` answers DejaVu Sans Mono, and nothing on a
//! Debian-family default install covers `⏵` U+23F5 (Claude Code's `⏵⏵ bypass
//! permissions on`), `⏺`, `⎿` or most of Misc Technical — so aterm SHIPS its own
//! stack there, with **JetBrains Mono** as the primary. The chain a default
//! Linux window resolves through is:
//!
//! ```text
//! JetBrains Mono (real Bold / Italic / BoldItalic faces)      primary
//!  → DejaVu Sans Mono (embedded)                               broad tier, first
//!  → system per-script / CJK faces                             broad tier
//!  → Noto Sans Symbols 2 → Noto Sans Math (embedded)           symbol tier, first
//!  → system symbol faces                                       symbol tier
//!  → colour emoji → Symbols Nerd Font (embedded, runtime tier)
//!  → procedural Synthetic → .notdef
//! ```
//!
//! Every face here is `include_bytes!`'d, so the stack needs no I/O and works
//! identically on a stripped container and a full desktop.
//!
//! # Identity
//!
//! A bundled face has no path. It is named by a VIRTUAL identity under the
//! `bundled:` scheme (the `display:` scheme's twin), which is what
//! [`crate::resolve_config_font`], [`crate::font_catalog::resolve_and_admit`],
//! [`crate::primary_font_candidate_paths`] and a renderer's `primary_path`
//! carry. The unconfigured default IS the identity, so it costs no directory
//! scan and is the same face on every machine.
//!
//! A user who TYPES the family name — `font_family = "JetBrains Mono"` — gets an
//! INSTALLED JetBrains Mono when one resolves (exactly what the name meant
//! before the stack existed, so the variable `JetBrainsMono[wght].ttf` keeps its
//! `font_weight` / `wght` axis) and the bundled face only when none does
//! ([`crate::resolve_family_or_bundled`]). Every consumer of a resolved identity
//! reads its bytes through [`crate::read_resolved_font`], never as a file.
//!
//! # Why not macOS
//!
//! The macOS default (SF Mono, W9-instantiated, plus Apple Symbols / STIX Two
//! Math in the symbol tier) is unchanged by the decision, so on macOS nothing
//! here would ever be selected by default — and a universal binary would carry
//! the ~2.8 MB twice. [`ACTIVE`] is therefore `false` there and every function
//! below answers "no bundled face", reducing each call site exactly to its
//! pre-stack behaviour.

/// Whether the bundled stack is compiled into this build.
pub const ACTIVE: bool = cfg!(all(
    feature = "bundled-font-stack",
    not(target_os = "macos")
));

/// The bundled primary's family name, as a user types it.
pub const PRIMARY_FAMILY: &str = "JetBrains Mono";

/// The virtual identity scheme for bundled faces.
pub const SCHEME: &str = "bundled:";

/// The bundled primary's virtual identity (regular face).
pub const PRIMARY_ID: &str = "bundled:JetBrains Mono";
/// The bundled primary's BOLD face identity.
pub const PRIMARY_BOLD_ID: &str = "bundled:JetBrains Mono Bold";
/// The bundled primary's ITALIC face identity.
pub const PRIMARY_ITALIC_ID: &str = "bundled:JetBrains Mono Italic";
/// The bundled primary's BOLD-ITALIC face identity.
pub const PRIMARY_BOLD_ITALIC_ID: &str = "bundled:JetBrains Mono Bold Italic";
/// The embedded DejaVu Sans Mono, as a BROAD-tier chain entry.
pub const DEJAVU_ID: &str = "bundled:DejaVu Sans Mono";
/// Noto Sans Symbols 2, as a SYMBOL-tier chain entry.
pub const NOTO_SYMBOLS2_ID: &str = "bundled:Noto Sans Symbols 2";
/// Noto Sans Math, as a SYMBOL-tier chain entry.
pub const NOTO_MATH_ID: &str = "bundled:Noto Sans Math";

#[cfg(all(feature = "bundled-font-stack", not(target_os = "macos")))]
mod bytes {
    pub(super) static JBM_REGULAR: &[u8] =
        include_bytes!("../assets/bundled/JetBrainsMono-Regular.ttf");
    pub(super) static JBM_BOLD: &[u8] = include_bytes!("../assets/bundled/JetBrainsMono-Bold.ttf");
    pub(super) static JBM_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/JetBrainsMono-Italic.ttf");
    pub(super) static JBM_BOLD_ITALIC: &[u8] =
        include_bytes!("../assets/bundled/JetBrainsMono-BoldItalic.ttf");
    pub(super) static NOTO_SYMBOLS2: &[u8] =
        include_bytes!("../assets/bundled/NotoSansSymbols2-Regular.ttf");
    pub(super) static NOTO_MATH: &[u8] =
        include_bytes!("../assets/bundled/NotoSansMath-Regular.ttf");
}

/// The bytes behind one virtual identity, or `None` for anything that is not a
/// bundled identity in THIS build.
#[cfg(all(feature = "bundled-font-stack", not(target_os = "macos")))]
#[must_use]
pub fn face_for_id(id: &str) -> Option<&'static [u8]> {
    match id {
        PRIMARY_ID => Some(bytes::JBM_REGULAR),
        PRIMARY_BOLD_ID => Some(bytes::JBM_BOLD),
        PRIMARY_ITALIC_ID => Some(bytes::JBM_ITALIC),
        PRIMARY_BOLD_ITALIC_ID => Some(bytes::JBM_BOLD_ITALIC),
        DEJAVU_ID => Some(crate::embedded_font()),
        NOTO_SYMBOLS2_ID => Some(bytes::NOTO_SYMBOLS2),
        NOTO_MATH_ID => Some(bytes::NOTO_MATH),
        _ => None,
    }
}

/// The bytes behind one virtual identity: none, the stack being compiled out.
#[cfg(not(all(feature = "bundled-font-stack", not(target_os = "macos"))))]
#[must_use]
pub fn face_for_id(_id: &str) -> Option<&'static [u8]> {
    None
}

/// Whether `path` is a bundled virtual identity in this build.
#[must_use]
pub fn is_bundled_id(path: &str) -> bool {
    face_for_id(path).is_some()
}

/// Whether `family` is spelled as a bundled virtual IDENTITY (`bundled:…`)
/// rather than a family name — the spelling that always means the compiled-in
/// face, never an installed one.
#[must_use]
pub fn is_identity(family: &str) -> bool {
    family.trim().starts_with(SCHEME)
}

/// Resolve a config-authored FAMILY request to a bundled face: its virtual
/// identity and bytes. Matches the identity itself (`bundled:JetBrains Mono`)
/// and the family NAME a user types, case- and separator-insensitively
/// (`JetBrains Mono`, `jetbrains-mono`, `JetBrainsMono`), plus the three styled
/// spellings (`JetBrains Mono Bold` / `Italic` / `Bold Italic`) so the
/// `font_family_bold`/`_italic`/`_bold_italic` keys can name them too. The
/// fallback-only faces (DejaVu, Noto) resolve by identity only: a family name
/// such as "DejaVu Sans Mono" keeps meaning the INSTALLED face, as before.
#[must_use]
pub fn face_for_family(family: &str) -> Option<(&'static str, &'static [u8])> {
    let trimmed = family.trim();
    for id in [
        PRIMARY_ID,
        PRIMARY_BOLD_ID,
        PRIMARY_ITALIC_ID,
        PRIMARY_BOLD_ITALIC_ID,
        DEJAVU_ID,
        NOTO_SYMBOLS2_ID,
        NOTO_MATH_ID,
    ] {
        if trimmed == id {
            return face_for_id(id).map(|bytes| (id, bytes));
        }
    }
    let id = match normalize(trimmed).as_str() {
        "jetbrainsmono" | "jetbrainsmonoregular" => PRIMARY_ID,
        "jetbrainsmonobold" => PRIMARY_BOLD_ID,
        "jetbrainsmonoitalic" => PRIMARY_ITALIC_ID,
        "jetbrainsmonobolditalic" => PRIMARY_BOLD_ITALIC_ID,
        _ => return None,
    };
    face_for_id(id).map(|bytes| (id, bytes))
}

/// The bundled PRIMARY-family face for a request, when the request names the
/// primary family (any of its four styles) — the display-face interception's
/// twin for [`crate::Renderer::from_system_with_family`].
#[must_use]
pub fn primary_for_family(family: &str) -> Option<(&'static str, &'static [u8])> {
    face_for_family(family).filter(|(id, _)| {
        matches!(
            *id,
            PRIMARY_ID | PRIMARY_BOLD_ID | PRIMARY_ITALIC_ID | PRIMARY_BOLD_ITALIC_ID
        )
    })
}

/// The real `[bold, italic, bold-italic]` faces of a bundled primary, keyed by
/// the primary's identity — what `Renderer::ensure_styled_faces` fills its
/// slots from instead of deriving sibling FILE paths. Empty for any other
/// primary.
#[must_use]
pub fn styled_siblings(primary: &str) -> [Option<&'static [u8]>; 3] {
    if primary == PRIMARY_ID {
        [
            face_for_id(PRIMARY_BOLD_ID),
            face_for_id(PRIMARY_ITALIC_ID),
            face_for_id(PRIMARY_BOLD_ITALIC_ID),
        ]
    } else {
        [None; 3]
    }
}

/// The DEFAULT primary candidate: the bundled JetBrains Mono identity, placed
/// ahead of the built-in system `FONT_CANDIDATES` (so a system DejaVu Sans Mono
/// no longer wins by default) and behind any configured family (so a user's
/// choice still wins). Empty when the stack is inactive.
#[must_use]
pub fn default_primary_candidates() -> &'static [&'static str] {
    if ACTIVE { &[PRIMARY_ID] } else { &[] }
}

/// The bundled BROAD-tier chain entries, placed after the user's configured
/// `fallback_fonts` and ahead of every system discovery path. Additive: the
/// scan keeps going past them to the system per-script faces and backstop.
#[must_use]
pub fn broad_fallback_ids() -> &'static [&'static str] {
    if ACTIVE { &[DEJAVU_ID] } else { &[] }
}

/// The bundled SYMBOL-tier chain entries, in order, placed after a configured
/// `symbol_font` and ahead of the system symbol faces. Additive.
#[must_use]
pub fn symbol_fallback_ids() -> &'static [&'static str] {
    if ACTIVE {
        &[NOTO_SYMBOLS2_ID, NOTO_MATH_ID]
    } else {
        &[]
    }
}

/// The FILE NAMES of the system faces the bundled symbol tier already
/// carries — the same upstream Noto files, compiled in.
const SUPERSEDED_SYSTEM_FILES: &[&str] =
    &["NotoSansSymbols2-Regular.ttf", "NotoSansMath-Regular.ttf"];

/// Whether a built-in SYSTEM candidate `path` names a face the bundled stack
/// already serves ahead of it, by file name. Such an entry can only ever
/// answer a code point the bundled copy answered first, so the discovery list
/// drops it — and with it the font-tree walk that relocating a missing copy
/// used to start on every launch of a host that does not install it.
#[must_use]
pub fn supersedes_system_face(path: &str) -> bool {
    ACTIVE
        && std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| SUPERSEDED_SYSTEM_FILES.contains(&n))
}

/// Families the bundled stack makes selectable by NAME (for `list-fonts`).
#[must_use]
pub fn selectable_families() -> &'static [&'static str] {
    if ACTIVE { &[PRIMARY_FAMILY] } else { &[] }
}

/// The resolver's family normalization (lowercase, no whitespace / `-` / `_`).
fn normalize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| !ch.is_whitespace() && *ch != '-' && *ch != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_names_resolve_to_the_bundled_primary_only_when_active() {
        for spelling in [
            "JetBrains Mono",
            "jetbrains-mono",
            "JetBrainsMono",
            " JETBRAINS MONO ",
        ] {
            let hit = face_for_family(spelling);
            assert_eq!(hit.is_some(), ACTIVE, "{spelling:?}");
            if let Some((id, _)) = hit {
                assert_eq!(id, PRIMARY_ID);
            }
        }
        assert_eq!(
            face_for_family("JetBrains Mono Bold Italic").map(|(id, _)| id),
            ACTIVE.then_some(PRIMARY_BOLD_ITALIC_ID)
        );
        // An installed family keeps meaning the installed face.
        assert!(face_for_family("DejaVu Sans Mono").is_none());
        assert!(face_for_family("JetBrains Mono NL").is_none());
        assert!(face_for_family("").is_none());
    }

    #[test]
    fn every_bundled_face_parses_and_the_primary_is_a_real_monospace_family() {
        if !ACTIVE {
            return;
        }
        for id in [
            PRIMARY_ID,
            PRIMARY_BOLD_ID,
            PRIMARY_ITALIC_ID,
            PRIMARY_BOLD_ITALIC_ID,
            DEJAVU_ID,
            NOTO_SYMBOLS2_ID,
            NOTO_MATH_ID,
        ] {
            let bytes = face_for_id(id).expect("active stack serves every id");
            let face = ttf_parser::Face::parse(bytes, 0).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(face.number_of_glyphs() > 100, "{id}");
        }
        let weights: Vec<(u16, bool)> = [
            PRIMARY_ID,
            PRIMARY_BOLD_ID,
            PRIMARY_ITALIC_ID,
            PRIMARY_BOLD_ITALIC_ID,
        ]
        .iter()
        .map(|id| {
            let f = ttf_parser::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
            assert!(f.is_monospaced(), "{id} must be monospaced");
            (f.weight().to_number(), f.is_italic())
        })
        .collect();
        assert_eq!(
            weights,
            [(400, false), (700, false), (400, true), (700, true)]
        );
    }

    // ---- the stack, bound to the real renderer ------------------------------

    use crate::{FaceId, GlyphClass, Renderer, Theme};

    /// The default renderer a GUI window builds when nothing is configured,
    /// sealed exactly as the backend worker seals it.
    fn default_sealed() -> Option<Renderer> {
        if !ACTIVE {
            eprintln!("SKIP: bundled stack inactive");
            return None;
        }
        let mut r = Renderer::from_system(18.0, Theme::default()).expect("a default face");
        r.seal_admitted_font_sources();
        Some(r)
    }

    /// Where `ch` resolves in the sealed default chain, as a readable label:
    /// the bundled identity or system path of the face that draws it.
    fn resolved_face(r: &mut Renderer, ch: char) -> String {
        let key = r.glyph_key(ch);
        match key.source {
            FaceId::Primary if key.glyph_class == GlyphClass::MonoGid => {
                format!("primary:{}", r.primary_source_path().unwrap_or("-"))
            }
            FaceId::Primary => "NOTDEF".to_string(),
            FaceId::Fallback => format!(
                "fallback:{}",
                r.fallback_pick
                    .get(&ch)
                    .and_then(|&i| r.fallback_chain.get(i))
                    .and_then(|f| f.path.clone())
                    .unwrap_or_default()
            ),
            FaceId::SymbolFallback => format!(
                "symbol:{}",
                r.symbol_chain_pick(ch)
                    .and_then(|i| r.symbol_chain.get(i))
                    .and_then(|f| f.path.clone())
                    .unwrap_or_default()
            ),
            other => format!("{other:?}"),
        }
    }

    /// DEFAULT SELECTION: with nothing configured, the primary is the bundled
    /// JetBrains Mono — not the system DejaVu Sans Mono `fc-match monospace`
    /// answers, which is the first system candidate and used to win.
    #[test]
    fn the_default_primary_is_the_bundled_jetbrains_mono() {
        if !ACTIVE {
            eprintln!("SKIP: bundled stack inactive");
            return;
        }
        let r = Renderer::from_system(18.0, Theme::default()).expect("a default face");
        assert_eq!(r.primary_source_path(), Some(PRIMARY_ID));
        let (bytes, _) = r.chrome_primary_face().expect("primary bytes");
        assert_eq!(&bytes[..], face_for_id(PRIMARY_ID).unwrap());
        // The candidate list the off-thread config worker walks agrees.
        let candidates = crate::primary_font_candidate_paths();
        assert_eq!(candidates.first().map(String::as_str), Some(PRIMARY_ID));
    }

    /// A CONFIGURED family still wins over the bundled default, by name or by
    /// path; and the bundled family resolves by NAME through every resolver
    /// (startup, strict configured, config admission, the catalogue worker).
    #[test]
    fn a_configured_font_still_wins_and_the_bundled_family_resolves_by_name() {
        if !ACTIVE {
            return;
        }
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/DejaVuSansMono.ttf");
        let r = Renderer::from_system_with_family(Some(path), 18.0, Theme::default()).unwrap();
        assert_eq!(
            r.primary_source_path(),
            Some(path),
            "a configured path wins"
        );
        let r = Renderer::from_configured_font_family(path, 18.0, Theme::default()).unwrap();
        assert_eq!(r.primary_source_path(), Some(path));

        // The NAME yields to an installed JetBrains Mono (see
        // `resolve_family_or_bundled`, and the catalogue's fixture test for
        // that half); on a host with none, it is the bundled face everywhere.
        if crate::resolve_font_family(PRIMARY_FAMILY).is_some() {
            eprintln!("SKIP by-name half: JetBrains Mono is installed on this host");
            return;
        }
        for spelling in ["JetBrains Mono", PRIMARY_ID] {
            let r =
                Renderer::from_system_with_family(Some(spelling), 18.0, Theme::default()).unwrap();
            assert_eq!(r.primary_source_path(), Some(PRIMARY_ID), "{spelling}");
            let r =
                Renderer::from_configured_font_family(spelling, 18.0, Theme::default()).unwrap();
            assert_eq!(r.primary_source_path(), Some(PRIMARY_ID), "{spelling}");
            assert_eq!(
                crate::resolve_config_font(spelling).as_deref(),
                Ok(PRIMARY_ID)
            );
        }
        let batch = crate::font_catalog::resolve_and_admit(&[
            "JetBrains Mono".to_string(),
            "JetBrains Mono Bold".to_string(),
        ]);
        let admitted: Vec<_> = batch
            .entries
            .iter()
            .map(|e| e.result.as_ref().map(|a| a.path.clone()).ok())
            .collect();
        assert_eq!(
            admitted,
            [
                Some(PRIMARY_ID.to_string()),
                Some(PRIMARY_BOLD_ID.to_string())
            ]
        );
        assert!(crate::list_fonts().iter().any(|f| f == PRIMARY_FAMILY));
        let info = crate::face_info("JetBrains Mono").expect("face_info resolves the bundle");
        assert_eq!(info.path, PRIMARY_ID);
        // JetBrains Mono's own line (1.32 em: ascent 1020 + descent 300 per
        // 1000 upem, no lineGap) and 0.6 em advance, at the 16 px probe.
        assert_eq!((info.cell_width, info.cell_height), (10, 22));
    }

    /// BOLD / ITALIC RESOLUTION: the bundled primary's three styled slots are
    /// filled from its REAL compiled-in faces — never a sibling FILE lookup,
    /// never the synthetic dilation/shear.
    #[test]
    fn the_bundled_primary_resolves_real_bold_italic_and_bold_italic_faces() {
        let Some(r) = default_sealed() else { return };
        assert_eq!(r.debug_styled_face_indices(), [Some(0); 3]);
        let styles: Vec<(u16, bool)> = r
            .styled_faces
            .iter()
            .map(|slot| {
                let face = slot.as_ref().expect("slot filled");
                let parsed = ttf_parser::Face::parse(&face.bytes, 0).unwrap();
                (parsed.weight().to_number(), parsed.is_italic())
            })
            .collect();
        assert_eq!(styles, [(700, false), (400, true), (700, true)]);
    }

    /// FALLBACK ORDER over the owner's measured test set, on the SEALED default
    /// generation a window publishes. Each expectation is the first face in the
    /// declared order (JetBrains Mono → DejaVu Sans Mono → Noto Sans Symbols 2
    /// → Noto Sans Math → …) whose cmap carries the code point, and NOTHING in
    /// the set may end as `.notdef`.
    #[test]
    fn the_test_set_resolves_in_the_declared_order_and_never_to_notdef() {
        let Some(mut r) = default_sealed() else {
            return;
        };
        let primary = format!("primary:{PRIMARY_ID}");
        let dejavu = format!("fallback:{DEJAVU_ID}");
        let sym2 = format!("symbol:{NOTO_SYMBOLS2_ID}");
        let math = format!("symbol:{NOTO_MATH_ID}");
        let expect: &[(char, &str)] = &[
            // JetBrains Mono's own glyphs.
            ('▶', &primary),
            ('◀', &primary),
            ('●', &primary),
            ('✓', &primary),
            ('✗', &primary),
            ('❯', &primary),
            ('→', &primary),
            ('⚠', &primary),
            ('≡', &primary),
            ('∞', &primary),
            ('≈', &primary),
            // What JetBrains Mono lacks and DejaVu Sans Mono carries.
            ('◐', &dejavu),
            ('✔', &dejavu),
            ('✘', &dejavu),
            ('↵', &dejavu),
            ('★', &dejavu),
            ('⚙', &dejavu),
            // Media / technical: Noto Sans Symbols 2 — a REAL glyph now beats
            // the synthetic tier that drew these before the stack shipped.
            ('⏴', &sym2),
            ('⏵', &sym2),
            ('⏶', &sym2),
            ('⏷', &sym2),
            ('⏺', &sym2),
            ('⏸', &sym2),
            // Operators only the math face carries.
            ('⊨', &math),
            ('⨁', &math),
            ('⩽', &math),
            ('⟹', &math),
            ('ℒ', &math),
        ];
        let mut wrong = Vec::new();
        for &(ch, want) in expect {
            let got = resolved_face(&mut r, ch);
            if got != want {
                wrong.push(format!("{ch} U+{:04X}: want {want}, got {got}", ch as u32));
            }
        }
        assert!(
            wrong.is_empty(),
            "chain order violated:\n{}",
            wrong.join("\n")
        );
        // The rest of the set: braille is procedural, ⏩ is emoji-default (the
        // colour face's, or the synthesis without one — never a bundled text
        // face), and whatever carries ⎿ it must not be tofu.
        for ch in ['⎿', '⏩', '⠀', '⣿'] {
            let got = resolved_face(&mut r, ch);
            assert_ne!(got, "NOTDEF", "{ch} U+{:04X} is tofu", ch as u32);
            assert!(
                !got.starts_with("fallback:bundled") && !got.starts_with("symbol:bundled"),
                "{ch}: an incidental bundled pick ({got})"
            );
        }
    }

    /// The bundled faces lead their tiers but never TAKE what the host's own
    /// faces own: no emoji-default and no private-use point is ever picked
    /// from a bundled chain face (the colour face and the Symbols Nerd Font
    /// own those).
    #[test]
    fn a_bundled_chain_face_never_takes_an_emoji_default_or_private_use_point() {
        let Some(mut r) = default_sealed() else {
            return;
        };
        let mut checked = 0usize;
        for id in [DEJAVU_ID, NOTO_SYMBOLS2_ID, NOTO_MATH_ID] {
            let face = ttf_parser::Face::parse(face_for_id(id).unwrap(), 0).unwrap();
            let mut incidental = Vec::new();
            for sub in face.tables().cmap.unwrap().subtables {
                if sub.is_unicode() {
                    sub.codepoints(|cp| {
                        if let Some(ch) = char::from_u32(cp)
                            && (aterm_grapheme::is_emoji_presentation(ch)
                                || crate::font_chain::is_private_use(ch))
                        {
                            incidental.push(ch);
                        }
                    });
                }
            }
            for ch in incidental {
                // The PRIMARY answering is not a chain pick: a primary face's
                // own glyph always wins, bundled or installed, as it always has.
                let got = resolved_face(&mut r, ch);
                assert!(
                    got.starts_with("primary:") || !got.contains(SCHEME),
                    "U+{:04X} taken by {got}",
                    ch as u32
                );
                checked += 1;
            }
        }
        assert!(
            checked > 0,
            "no bundled face maps an incidental point; nothing proven"
        );
    }

    /// THE STARTUP / SETTINGS READ PATH: every consumer of a RESOLVED identity
    /// (`resolve_config_font`'s answer) reads it through `read_resolved_font`,
    /// which serves a bundled identity from compiled-in bytes. The GUI's
    /// startup styled-face admission (`font_family_bold = "JetBrains Mono
    /// Bold"`) and the Settings preview used to hand the identity to the
    /// filesystem and fail with ENOENT.
    #[test]
    fn a_resolved_bundled_identity_reads_as_its_compiled_in_bytes() {
        if !ACTIVE || crate::resolve_font_family(PRIMARY_FAMILY).is_some() {
            return;
        }
        for (name, id) in [
            ("JetBrains Mono", PRIMARY_ID),
            ("JetBrains Mono Bold", PRIMARY_BOLD_ID),
            ("JetBrains Mono Italic", PRIMARY_ITALIC_ID),
            ("JetBrains Mono Bold Italic", PRIMARY_BOLD_ITALIC_ID),
        ] {
            let resolved = crate::resolve_config_font(name).expect(name);
            assert_eq!(resolved, id);
            let bytes = crate::read_resolved_font(&resolved).expect(name);
            assert_eq!(&bytes[..], face_for_id(id).unwrap(), "{name}");
            assert!(
                crate::font_file::read_font_file(std::path::Path::new(&resolved)).is_err(),
                "the identity is not a file — reading it as one is the bug"
            );
        }
        // A real file still reads as a file.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/assets/DejaVuSansMono.ttf");
        assert_eq!(
            &crate::read_resolved_font(path).unwrap()[..],
            crate::embedded_font()
        );
    }

    /// THE OWNER'S ORDER puts the embedded Symbols Nerd Font AHEAD of Noto Sans
    /// Symbols 2 / Noto Sans Math. Its private-use icons were never contested;
    /// the six non-private-use points both carry (measured with fontTools:
    /// `⏻ ⏼ ⏽ ⏾ ☰ ⭘`) must not be drawn by a bundled Noto face.
    #[test]
    #[cfg(feature = "embedded-symbols")]
    fn the_nerd_font_precedes_the_bundled_noto_faces_where_both_draw() {
        // The measured set, pinned: a Nerd Font update that adds a text point
        // announces itself here.
        let owned: Vec<char> = (0u32..0x11_0000)
            .filter_map(char::from_u32)
            .filter(|&c| crate::embedded_symbols_own_text_point(c))
            .collect();
        assert_eq!(
            owned.iter().collect::<String>(),
            "⏻⏼⏽⏾☰♥⚡❬❭❮❯❰❱⭘",
            "the embedded Nerd Font's non-private-use coverage moved"
        );
        let Some(mut r) = default_sealed() else {
            return;
        };
        let contested = ['⏻', '⏼', '⏽', '⏾', '☰', '⭘'];
        for ch in contested {
            let noto_has = [NOTO_SYMBOLS2_ID, NOTO_MATH_ID].iter().any(|id| {
                ttf_parser::Face::parse(face_for_id(id).unwrap(), 0)
                    .unwrap()
                    .glyph_index(ch)
                    .is_some_and(|g| g.0 != 0)
            });
            assert!(noto_has, "{ch}: not contested — the test proves nothing");
            let got = resolved_face(&mut r, ch);
            assert!(
                !got.starts_with("symbol:bundled:Noto"),
                "{ch} U+{:04X}: a bundled Noto face took a Nerd Font point ({got})",
                ch as u32
            );
            assert_ne!(got, "NOTDEF", "{ch} is tofu");
        }
        // CONTROL: an uncontested point still comes from Noto Sans Symbols 2.
        assert_eq!(
            resolved_face(&mut r, '⏵'),
            format!("symbol:{NOTO_SYMBOLS2_ID}")
        );
    }
}
