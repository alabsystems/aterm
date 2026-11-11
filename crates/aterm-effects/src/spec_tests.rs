// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const TINY_TRIUMPHS: &str = include_str!("../toy-packs/community/tiny-triumphs/pack.toml");
const COMMUNITY_TOY_PACKS: &[(&str, &str)] = &[("tiny-triumphs", TINY_TRIUMPHS)];

#[test]
fn every_checked_in_community_toy_pack_compiles() {
    assert!(
        !COMMUNITY_TOY_PACKS.is_empty(),
        "the community gallery has an example"
    );
    for &(name, source) in COMMUNITY_TOY_PACKS {
        compile_toy_pack_toml(source).unwrap_or_else(|error| panic!("{name}:\n{error}"));
    }
}

#[test]
fn checked_in_toy_pack_compiles_through_real_dispatch_and_scanner() {
    let pack = compile_toy_pack_toml(TINY_TRIUMPHS).expect("example pack compiles");
    assert_eq!(pack.metadata().id, "community.aterm.tiny-triumphs");
    assert_eq!(pack.toy_count(), 3);
    assert_eq!(pack.word_count(), 5);

    let shipped = pack
        .spec_table()
        .override_for(form_hash("shipped"))
        .expect("shipped recipe registered");
    assert!(matches!(
        shipped.ink,
        Some(InkSpec {
            colorway: Colorway::TwoTone {
                c0: 0x0058_D8FF,
                c1: 0x00FF_73B9
            },
            sweep_once: true
        })
    ));
    assert_eq!(
        shipped.burst,
        Some(BurstSpec {
            kind: BurstKind::Starburst,
            chance_pct: 35
        })
    );
    let purrfect = pack
        .spec_table()
        .override_for(form_hash("purrfect"))
        .expect("cat recipe registered");
    assert!(purrfect.graphic.is_some());
    assert!(matches!(
        purrfect.ink,
        Some(InkSpec {
            colorway: Colorway::SelfGlow {
                window_ms: 1350,
                ..
            },
            sweep_once: true
        })
    ));

    let lexicon =
        aterm_lexicon::Lexicon::with_languages_and_override(&["en"], Some(pack.lexicon_toml()))
            .expect("compiled fragment parses");
    let matches = lexicon.scan(
        "shipit, then deepthink, that review was purrfect",
        &aterm_lexicon::ScanOptions::default(),
    );
    assert_eq!(
        matches.len(),
        3,
        "all pack surfaces reach the real scanner: {matches:?}"
    );
}

#[test]
fn toy_pack_schema_is_strict_about_unknown_fields_and_values() {
    let unknown_field =
        TINY_TRIUMPHS.replacen("chance_pct = 35", "chance_pct = 35, surprise = true", 1);
    compile_toy_pack_toml(&unknown_field).expect_err("unknown field rejected");

    let unknown_kind =
        TINY_TRIUMPHS.replacen("kind = \"starburst\"", "kind = \"confetti_cannon\"", 1);
    compile_toy_pack_toml(&unknown_kind).expect_err("unknown effect rejected");

    let bad_color = TINY_TRIUMPHS.replacen("#58D8FF", "58D8FF", 1);
    compile_toy_pack_toml(&bad_color).expect_err("non-canonical color rejected");

    let unscannable = TINY_TRIUMPHS.replacen("\"shipit\"", "\"ship it\"", 1);
    compile_toy_pack_toml(&unscannable).expect_err("multi-word trigger rejected");

    let cjk_exception = TINY_TRIUMPHS.replacen("\"shipit\"", "\"猫背\"", 1);
    compile_toy_pack_toml(&cjk_exception)
        .expect_err("a production-scanner CJK exception cannot become a trigger");
}

#[test]
fn toy_pack_semantics_collect_version_identity_budget_and_timing_errors() {
    let bad = r##"
schema = 7
[pack]
id = "not_namespaced"
name = "Bad Pack"
version = 0
authors = []
license = "Apache-2.0"

[[toy]]
id = "same"
lang = "x"
words = ["duplicate", " trailing "]
ink = { kind = "rainbow", drift_ms = 20, colors = ["#000000", "#ffffff"] }
burst = { kind = "glow", chance_pct = 101 }
graphic = { collection = "dragons", dwell_ms = [4000, 3000] }

[[toy]]
id = "same"
lang = "en"
words = ["DUPLICATE"]
ink = { kind = "self_glow", window_ms = 9000 }
"##;
    let error = compile_toy_pack_toml(bad).expect_err("semantic errors rejected");
    let mut fields: Vec<&str> = error
        .diagnostics()
        .iter()
        .map(|d| d.split_once(": ").map_or(d.as_str(), |(field, _)| field))
        .collect();
    fields.sort_unstable();
    let mut want = vec![
        "schema",
        "pack.id",
        "pack.version",
        "pack.authors",
        "toy[0] (\"same\").lang",
        "toy[0] (\"same\").ink.colors",
        "toy[0] (\"same\").ink.drift_ms",
        "toy[0] (\"same\").burst.chance_pct",
        "toy[0] (\"same\").graphic.collection",
        "toy[0] (\"same\").graphic.dwell_ms",
        "toy[0] (\"same\").words[1]",
        "toy[1] (\"same\").id",
        "toy[1] (\"same\").ink.window_ms",
        "toy[1] (\"same\").words[0]",
    ];
    want.sort_unstable();
    assert_eq!(fields, want, "{:#?}", error.diagnostics());
}

#[test]
fn toy_pack_source_cap_precedes_toml_parse() {
    let oversized = "x".repeat(MAX_TOY_PACK_BYTES + 1);
    let error = compile_toy_pack_toml(&oversized).expect_err("oversize source rejected");
    assert!(
        error.to_string().contains(&MAX_TOY_PACK_BYTES.to_string()),
        "{error}"
    );
}

#[test]
fn toy_pack_file_read_is_regular_utf8_and_bounded() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("at-toy-pack-")
        .suffix(".noindex")
        .tempdir()
        .expect("create reader fixture directory");
    let root = dir.path();

    let regular = root.join("regular.toml");
    std::fs::write(&regular, TINY_TRIUMPHS).expect("write regular fixture");
    assert_eq!(
        read_toy_pack_file(&regular).expect("read regular manifest"),
        TINY_TRIUMPHS
    );

    let directory_error = read_toy_pack_file(root).expect_err("directory rejected");
    assert_eq!(directory_error.kind(), std::io::ErrorKind::InvalidInput);

    let invalid_utf8 = root.join("invalid-utf8.toml");
    std::fs::write(&invalid_utf8, [0xff]).expect("write invalid UTF-8 fixture");
    let utf8_error = read_toy_pack_file(&invalid_utf8).expect_err("invalid UTF-8 rejected");
    assert_eq!(utf8_error.kind(), std::io::ErrorKind::InvalidData);

    let oversized = root.join("oversized.toml");
    std::fs::write(&oversized, vec![b'x'; MAX_TOY_PACK_BYTES + 32])
        .expect("write oversized fixture");
    let oversized_error =
        read_toy_pack_file(&oversized).expect_err("oversized manifest rejected by reader");
    assert_eq!(oversized_error.kind(), std::io::ErrorKind::InvalidData);
    assert!(
        oversized_error
            .to_string()
            .contains(&MAX_TOY_PACK_BYTES.to_string()),
        "{oversized_error}"
    );
}

#[test]
fn toy_pack_rejects_possessive_dispatch_collisions() {
    let colliding = TINY_TRIUMPHS.replacen(
        "words = [\"shipit\", \"shipped\"]",
        "words = [\"shipit\", \"shipit's\"]",
        1,
    );
    compile_toy_pack_toml(&colliding)
        .expect_err("base and possessive surface must not share a dispatch key");
}

#[test]
fn toy_pack_recipe_word_count_is_bounded() {
    let words = (0..=MAX_WORDS_PER_TOY)
        .map(|index| format!("\"toyword{index}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let source = format!(
        "schema = 1\n\
         [pack]\n\
         id = \"community.test.bounded\"\n\
         name = \"Bounded\"\n\
         version = 1\n\
         authors = [\"Test Author\"]\n\
         license = \"Apache-2.0\"\n\
         [[toy]]\n\
         id = \"too-many\"\n\
         lang = \"en\"\n\
         words = [{words}]\n\
         burst = {{ kind = \"glow\" }}\n"
    );
    compile_toy_pack_toml(&source).expect_err("word cap rejected");
}

#[test]
fn custom_fragment_parses_and_keys_all_hash_variants() {
    let frag = r#"
[[sparkle_words.custom]]
words = ["Ultrathink"]
ink = { colorway = "rainbow" }

[[sparkle_words.custom]]
words = ["猫神"]
ink = "twotone:#ff0000,#0000ff"
burst = { kind = "starburst", chance = 250 }
graphic = { collection = "cats" }
"#;
    let entries = parse_custom_toml(frag).expect("fragment parses");
    let (table, lex_frag) = build_custom(&entries);
    assert!(table.has_custom());

    let base = table
        .override_for(form_hash("ultrathink"))
        .expect("folded base hash registered");
    assert!(matches!(
        base.ink,
        Some(InkSpec {
            colorway: Colorway::Rainbow { .. },
            ..
        })
    ));
    for suf in ["'s", "\u{2019}s", "'", "\u{2019}"] {
        let mut k = String::from("ultrathink");
        k.push_str(suf);
        assert!(
            table.override_for(form_hash(&k)).is_some(),
            "possessive variant {k:?} registered"
        );
    }
    assert!(table.override_for(form_hash("Ultrathink")).is_none());

    let cjk = table
        .override_for(form_hash("猫神"))
        .expect("raw CJK hash registered");
    assert_eq!(
        cjk.ink,
        Some(InkSpec {
            colorway: Colorway::TwoTone {
                c0: 0x00FF_0000,
                c1: 0x0000_00FF
            },
            sweep_once: true
        })
    );
    assert_eq!(
        cjk.burst,
        Some(BurstSpec {
            kind: BurstKind::Starburst,
            chance_pct: 100
        })
    );
    assert!(cjk.graphic.is_some());

    assert!(lex_frag.contains("forms = [\"Ultrathink\"]"), "{lex_frag}");
    assert!(lex_frag.contains("cjk = true"), "{lex_frag}");
    assert!(lex_frag.contains("forms = [\"猫神\"]"), "{lex_frag}");

    let lx = aterm_lexicon::Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
        .expect("synthesized fragment parses");
    let o = aterm_lexicon::ScanOptions::default();
    assert_eq!(lx.scan("go ultrathink now", &o).len(), 1);
    assert_eq!(lx.scan("これは猫神です", &o).len(), 1);
}

#[test]
fn custom_twotone_requires_exactly_two_colors_and_runtime_falls_back() {
    let valid = "twotone:#112233,#445566";
    let extra = "twotone:#112233,#445566,#778899";
    assert!(custom_colorway_is_valid(valid));
    assert!(!custom_colorway_is_valid(extra));
    assert!(!custom_colorway_is_valid("twotone:#112233"));

    assert!(matches!(
        parse_colorway(valid),
        Colorway::TwoTone {
            c0: 0x0011_2233,
            c1: 0x0044_5566,
        }
    ));
    assert!(matches!(parse_colorway(extra), Colorway::Rainbow { .. }));
}

#[test]
fn spec_table_overlay_is_deterministic_and_later_wins() {
    let spec = |kind| WordEffectSpec {
        burst: Some(BurstSpec {
            kind,
            chance_pct: 100,
        }),
        ..WordEffectSpec::default()
    };
    let mut base = SpecTable::default();
    base.insert_custom("shared", spec(BurstKind::Sparkle));
    base.insert_custom("base-only", spec(BurstKind::Nova));
    let mut later = SpecTable::default();
    later.insert_custom("shared", spec(BurstKind::Glow));
    later.insert_custom("later-only", spec(BurstKind::Starburst));

    base.overlay(later);
    let kind = |word: &str| {
        base.override_for(form_hash(&fold(word)))
            .and_then(|resolved| resolved.burst)
            .map(|burst| burst.kind)
    };
    assert_eq!(kind("shared"), Some(BurstKind::Glow));
    assert_eq!(kind("shared's"), Some(BurstKind::Glow));
    assert_eq!(kind("base-only"), Some(BurstKind::Nova));
    assert_eq!(kind("later-only"), Some(BurstKind::Starburst));
}

/// A single-char CJK word (needs `cjk_single_char = true`) and a mixed-script
/// word both surface on the lexicon `conflicts` channel.
#[test]
fn unscannable_custom_words_warn_on_the_conflicts_channel() {
    let frag = r#"
[[sparkle_words.custom]]
words = ["犬", "abc猫"]
ink = { colorway = "rainbow" }
"#;
    let entries = parse_custom_toml(frag).expect("fragment parses");
    let (table, lex_frag) = build_custom(&entries);
    assert!(table.has_custom());
    assert!(table.override_for(form_hash("犬")).is_some());
    let lx = aterm_lexicon::Lexicon::with_languages_and_override(&["en"], Some(&lex_frag))
        .expect("synthesized fragment parses");
    for word in ["\"犬\"", "\"abc猫\""] {
        assert!(
            lx.conflicts().iter().any(|c| c.contains(word)),
            "{word} does not warn, got {:?}",
            lx.conflicts()
        );
    }
}

#[test]
fn empty_spec_and_malformed_fragment_fail_safely() {
    let entries =
        parse_custom_toml("[[sparkle_words.custom]]\nwords = [\"zzz\"]\n").expect("parses");
    let (table, _) = build_custom(&entries);
    assert!(!table.has_custom());
    assert!(parse_custom_toml("[[sparkle_words.custom]").is_err());
    assert!(parse_custom_toml("").expect("empty ok").is_empty());
}

#[test]
fn legacy_inline_customs_keep_their_lenient_compatibility_semantics() {
    let entries = parse_custom_toml(
        "[[sparkle_words.custom]]\nwords = [\"legacy\"]\n\
         ink = { colorway = \"unknown-old-value\" }\n\
         burst = { kind = \"unknown-old-value\", chance = 900 }\n\
         graphic = { collection = \"unknown-old-value\" }\n",
    )
    .expect("legacy fragment still parses");
    let spec = entries[0].to_spec();
    assert!(matches!(
        spec.ink,
        Some(InkSpec {
            colorway: Colorway::Rainbow { .. },
            ..
        })
    ));
    assert_eq!(
        spec.burst,
        Some(BurstSpec {
            kind: BurstKind::Starburst,
            chance_pct: 100
        })
    );
    assert_eq!(
        spec.graphic,
        Some(GraphicSpec {
            collection: Collection::Cats,
            dwell_ms: (2200, 3598)
        })
    );
}

#[test]
fn consumer_capabilities_name_only_actual_shared_tuning_consumers() {
    let mut table = SpecTable::default();
    let burst = |kind, chance_pct| WordEffectSpec {
        burst: Some(BurstSpec { kind, chance_pct }),
        ..WordEffectSpec::default()
    };

    table.insert_custom(
        "graphic-only",
        WordEffectSpec {
            graphic: Some(GraphicSpec {
                collection: Collection::Cats,
                dwell_ms: (2_200, 3_598),
            }),
            ..WordEffectSpec::default()
        },
    );
    table.insert_custom("glow", burst(BurstKind::Glow, 100));
    table.insert_custom("supernova", burst(BurstKind::SuperNova, 100));
    table.insert_custom("rolled-off-sparkle", burst(BurstKind::Sparkle, 0));
    table.insert_custom(
        "self-glow",
        WordEffectSpec {
            ink: Some(InkSpec {
                colorway: Colorway::SelfGlow {
                    lift: 0.2,
                    amp: 0.3,
                    window_ms: 800,
                },
                sweep_once: true,
            }),
            ..WordEffectSpec::default()
        },
    );
    assert_eq!(
        table.consumer_capabilities(),
        SpecConsumerCapabilities::default(),
        "unrelated admitted recipes must not make shared controls look live"
    );

    table.insert_custom("nova", burst(BurstKind::Nova, 1));
    table.insert_custom("sparkle", burst(BurstKind::Sparkle, 1));
    table.insert_custom("starburst", burst(BurstKind::Starburst, 100));
    table.insert_custom(
        "rainbow",
        WordEffectSpec {
            ink: Some(rainbow_ink()),
            ..WordEffectSpec::default()
        },
    );
    table.insert_custom(
        "nova-twotone-no-burst",
        WordEffectSpec {
            ink: Some(InkSpec {
                colorway: Colorway::TwoTone {
                    c0: 0x0011_2233,
                    c1: 0x0044_5566,
                },
                sweep_once: false,
            }),
            burst: Some(BurstSpec {
                kind: BurstKind::Nova,
                chance_pct: 0,
            }),
            ..WordEffectSpec::default()
        },
    );
    assert_eq!(
        table.consumer_capabilities(),
        SpecConsumerCapabilities {
            nova_burst: true,
            sparkle_or_starburst_burst: true,
            rainbow_ink: true,
            twotone_ink: true,
            nova_twotone_ink: true,
            emphasis_class_default: false,
        }
    );

    let mut imported = SpecTable::default();
    imported.insert_custom("shared", burst(BurstKind::Sparkle, 100));
    let mut inline = SpecTable::default();
    inline.insert_custom(
        "shared",
        WordEffectSpec {
            graphic: Some(GraphicSpec {
                collection: Collection::Cats,
                dwell_ms: (2_200, 3_598),
            }),
            ..WordEffectSpec::default()
        },
    );
    imported.overlay(inline);
    assert_eq!(
        imported.consumer_capabilities(),
        SpecConsumerCapabilities::default(),
        "a fully shadowed Toy Pack burst is not an admitted consumer"
    );
}
