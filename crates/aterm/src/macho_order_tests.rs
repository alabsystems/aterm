// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn build_minimal_macho64(
    filetype: u32,
    text_vmaddr: u64,
    text_size: u64,
    symbols: &[(&str, u8, u64)],
) -> Vec<u8> {
    let ncmds: u32 = 2;
    let seg_cmd_size: u32 = 72 + 80 * 2;
    let sym_cmd_size: u32 = 24;
    let sizeofcmds = seg_cmd_size + sym_cmd_size;
    let mut out = Vec::new();

    out.extend_from_slice(&MH_MAGIC_64.to_le_bytes());
    out.extend_from_slice(&0x0100_000c_u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&filetype.to_le_bytes());
    out.extend_from_slice(&ncmds.to_le_bytes());
    out.extend_from_slice(&sizeofcmds.to_le_bytes());
    out.extend_from_slice(&0x2000u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    out.extend_from_slice(&LC_SEGMENT_64.to_le_bytes());
    out.extend_from_slice(&seg_cmd_size.to_le_bytes());
    out.extend_from_slice(&[0u8; 16]);
    out.extend_from_slice(&text_vmaddr.to_le_bytes());
    out.extend_from_slice(&text_size.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes());
    out.extend_from_slice(&text_size.to_le_bytes());
    out.extend_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(&7u32.to_le_bytes());
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    let push_sect = |buf: &mut Vec<u8>, seg: &[u8], sect: &[u8], addr: u64, sz: u64| {
        let mut sect_field = [0u8; 16];
        sect_field[..sect.len()].copy_from_slice(sect);
        let mut seg_field = [0u8; 16];
        seg_field[..seg.len()].copy_from_slice(seg);
        buf.extend_from_slice(&sect_field);
        buf.extend_from_slice(&seg_field);
        buf.extend_from_slice(&addr.to_le_bytes());
        buf.extend_from_slice(&sz.to_le_bytes());
        buf.extend_from_slice(&[0u8; 32]);
    };
    push_sect(&mut out, b"__TEXT", b"__text", text_vmaddr, text_size);
    push_sect(&mut out, b"__DATA", b"__data", text_vmaddr + text_size, 64);

    let actual_symoff = 32 + sizeofcmds;
    let nsyms = u32::try_from(symbols.len()).expect("test symbol count fits u32");
    let stroff = actual_symoff + nsyms * 16;

    let mut strtab = vec![0u8];
    let mut sym_records = Vec::new();
    for &(name, sect, addr) in symbols {
        let strx = u32::try_from(strtab.len()).expect("strx fits u32");
        strtab.extend_from_slice(name.as_bytes());
        strtab.push(0);
        sym_records.extend_from_slice(&strx.to_le_bytes());
        sym_records.push(N_SECT);
        sym_records.push(sect);
        sym_records.extend_from_slice(&0u16.to_le_bytes());
        sym_records.extend_from_slice(&addr.to_le_bytes());
    }
    let strsize = u32::try_from(strtab.len()).expect("strsize fits u32");

    out.extend_from_slice(&LC_SYMTAB.to_le_bytes());
    out.extend_from_slice(&sym_cmd_size.to_le_bytes());
    out.extend_from_slice(&actual_symoff.to_le_bytes());
    out.extend_from_slice(&nsyms.to_le_bytes());
    out.extend_from_slice(&stroff.to_le_bytes());
    out.extend_from_slice(&strsize.to_le_bytes());

    assert_eq!(out.len(), actual_symoff as usize);
    out.extend_from_slice(&sym_records);
    assert_eq!(out.len(), stroff as usize);
    out.extend_from_slice(&strtab);
    out
}

#[test]
fn v0_classifier_groups_generics_and_drops_with_instantiating_module_and_isolates_cold() {
    let live_mod =
        classify_symbol("__RNvMs4_NtCseA0bJGRgonR_9aterm_gui13rewind_workerNtB5_5Cache4keep");
    assert_eq!(
        live_mod,
        LocalityKey {
            tier: SymbolTier::Live,
            crate_name: "aterm_gui",
            module_name: "rewind_worker",
            sub_tier: 0,
        }
    );

    let root_method =
        classify_symbol("__RNvMsL_CseA0bJGRgonR_9aterm_guiNtB5_3App26active_tab_contains_native");
    assert_eq!(
        root_method,
        LocalityKey {
            tier: SymbolTier::Live,
            crate_name: "aterm_gui",
            module_name: "",
            sub_tier: 0,
        }
    );

    let generic_vec = classify_symbol(
        "__RNvMNtCsdzX4Z8B0DGU_5alloc3vecINtB2_3VecAhj20_E13with_capacityCs6mxZS6V2z2a_8aterm_db",
    );
    assert_eq!(
        generic_vec,
        LocalityKey {
            tier: SymbolTier::Live,
            crate_name: "aterm_db",
            module_name: "",
            sub_tier: 0,
        }
    );

    let thread_entry = classify_symbol(
        "__RINvNtNtCs6sTpqAl7fVb_3std3sys9backtrace28___rust_begin_short_backtraceNCINvMs0_NtCseA0bJGRgonR_9aterm_gui13rewind_workerNtB1g_12RewindWorker5start0E0uEB1i_",
    );
    assert_eq!(
        thread_entry,
        LocalityKey {
            tier: SymbolTier::Live,
            crate_name: "aterm_gui",
            module_name: "rewind_worker",
            sub_tier: 0,
        }
    );

    let drop_sym = classify_symbol(
        "__RINvNtCsedNDddgVRmw_4core3ptr9drop_glueINtNtB4_6option6OptionNtNtCseA0bJGRgonR_9aterm_gui10app_config11FleetConfigEECs7IiBsUA9gVZ_5aterm",
    );
    assert_eq!(
        drop_sym,
        LocalityKey {
            tier: SymbolTier::Live,
            crate_name: "aterm_gui",
            module_name: "app_config",
            sub_tier: 1,
        }
    );

    let cold_sym = classify_symbol("__RNvNtCseA0bJGRgonR_9aterm_gui8temporal10unnumbered.cold");
    assert_eq!(cold_sym.tier, SymbolTier::Cold);
}

#[test]
fn macho_object_and_bsd_archive_preserve_in_module_address_order_and_filter_non_text() {
    let obj = build_minimal_macho64(
        MH_OBJECT,
        0,
        0x100,
        &[
            ("ltmp0", 1, 0x0),
            ("__RNvNtCs1_3agh3cli3run", 1, 0x10),
            (
                "__RINvNtCs2_4core3ptr9drop_glueNtNtCs3_9aterm_gui5spawn4PaneE",
                1,
                0x20,
            ),
            ("__RNvNtCs3_9aterm_gui5spawn10second_fn_", 1, 0x40),
            ("__RNvNtCs3_9aterm_gui5spawn9first_fn_", 1, 0x30),
            ("__RNvNtCs3_9aterm_gui5spawn9first_fn_.cold", 1, 0x50),
            ("_writable_global", 2, 0x0),
            ("_main", 1, 0x60),
        ],
    );

    let ext_name = b"aterm_test_long_member.rcgu.o\0\0\0";
    let member_size = ext_name.len() + obj.len();
    let mut archive = Vec::from(&b"!<arch>\n"[..]);
    let hdr_name = format!("#1/{:<13}", ext_name.len());
    let hdr_rest = format!(
        "{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
        0, 0, 0, "100644", member_size
    );
    archive.extend_from_slice(hdr_name.as_bytes());
    archive.extend_from_slice(hdr_rest.as_bytes());
    archive.extend_from_slice(ext_name);
    archive.extend_from_slice(&obj);

    let ordered = build_order_list_from_slices(&[&archive]);
    assert_eq!(
        ordered,
        vec![
            "_main",
            "__RNvNtCs1_3agh3cli3run",
            "__RNvNtCs3_9aterm_gui5spawn9first_fn_",
            "__RNvNtCs3_9aterm_gui5spawn10second_fn_",
            "__RINvNtCs2_4core3ptr9drop_glueNtNtCs3_9aterm_gui5spawn4PaneE",
            "__RNvNtCs3_9aterm_gui5spawn9first_fn_.cold",
        ]
    );
}

#[test]
fn executable_fault_intersection_promotes_hot_symbols_and_rejects_random_neighbors() {
    let exe = build_minimal_macho64(
        MH_EXECUTE,
        0x1_0000_0000,
        0x1_0000,
        &[
            ("__mh_execute_header", 1, 0x1_0000_0000),
            ("_cold_page0", 1, 0x1_0000_0100),
            ("_hot_init", 1, 0x1_0000_4000),
            ("_cold_neighbor", 1, 0x1_0000_4200),
            ("_cold_page2", 1, 0x1_0000_8000),
            ("_tail_edge", 1, u64::MAX),
        ],
    );
    let mut faulted = HashSet::new();
    faulted.insert(0x1_0000_4000);
    let hits0 = extract_executable_faulted_symbols(&exe, &faulted);
    assert!(hits0.contains("_hot_init"));
    assert!(hits0.contains("_cold_neighbor"));
    assert!(!hits0.contains("_cold_page0"));
    assert!(!hits0.contains("__mh_execute_header"));

    let base = vec![
        "_main".to_string(),
        "_cold_page0".to_string(),
        "_cold_neighbor".to_string(),
        "_hot_init".to_string(),
        "_cold_page2".to_string(),
    ];
    let spacers = vec!["__atpad_t_00000".to_string(), "__atpad_c_00000".to_string()];
    let perm = permute_order_list_with_spacers(&base, &spacers, 0x9e37_79b9_7f4a_7c15);
    assert_eq!(perm.len(), base.len() + spacers.len());

    // Across 4 probes, `_hot_init` faults every time while `_cold_neighbor` only shared
    // a page in probe 0.
    let hits_only_hot: HashSet<String> = ["_hot_init".to_string()].into_iter().collect();
    let refined = refine_order_with_probe_hits(
        &base,
        &[
            hits0,
            hits_only_hot.clone(),
            hits_only_hot.clone(),
            hits_only_hot,
        ],
    )
    .expect("four hit sets refine an order");
    assert_eq!(
        refined,
        vec![
            "_main",
            "_hot_init",
            "_cold_page0",
            "_cold_neighbor",
            "_cold_page2",
        ]
    );

    let top_exec = build_minimal_macho64(
        MH_EXECUTE,
        u64::MAX - 64,
        64,
        &[("_top_page", 1, u64::MAX - 2)],
    );
    let top_pages: HashSet<u64> = [0x1000_0000].into_iter().collect();
    assert!(extract_executable_faulted_symbols(&top_exec, &top_pages).is_empty());
    let hit_top_pages: HashSet<u64> = [ARM64_PAGE_MASK].into_iter().collect();
    assert!(extract_executable_faulted_symbols(&top_exec, &hit_top_pages).contains("_top_page"));
}

#[test]
fn linker_arg_collection_and_order_file_roundtrip() {
    let scratch_dir = aterm_tempfile::Builder::new()
        .prefix("aterm-macho-order-test-")
        .suffix(".noindex")
        .tempdir()
        .expect("create scratch dir");
    let scratch = scratch_dir.path();

    let obj_a = scratch.join("alpha.o");
    let obj_b = scratch.join("beta.o");
    std::fs::write(
        &obj_a,
        build_minimal_macho64(MH_OBJECT, 0, 64, &[("__RNvNtCs1_3agh3cli3run", 1, 0x0)]),
    )
    .expect("write alpha.o");
    std::fs::write(
        &obj_b,
        build_minimal_macho64(MH_OBJECT, 0, 64, &[("_main", 1, 0x0)]),
    )
    .expect("write beta.o");

    let filelist = scratch.join("inputs.filelist");
    std::fs::write(&filelist, format!("{}\n", obj_b.display())).expect("write filelist");
    let resp = scratch.join("link.args");
    let quoted = |p: &Path| p.display().to_string().replace('\\', "\\\\");
    std::fs::write(
        &resp,
        format!(
            "-o \"{}\" \"{}\" -filelist \"{}\"\n",
            quoted(&scratch.join("out.o")),
            quoted(&obj_a),
            quoted(&filelist)
        ),
    )
    .expect("write response file");

    let args = vec![format!("@{}", resp.display()), "-x".into()];
    let inputs = collect_input_paths(&args);
    assert_eq!(inputs, vec![obj_a, obj_b]);

    let ordered = build_order_list(&inputs);
    assert_eq!(ordered, vec!["_main", "__RNvNtCs1_3agh3cli3run"]);

    let order_out = scratch.join("aterm.order");
    write_order_file(&order_out, &ordered).expect("write order file");
    let written = std::fs::read_to_string(&order_out).expect("read order file");
    assert_eq!(written, "_main\n__RNvNtCs1_3agh3cli3run\n");

    let pad_obj = scratch.join("spacer.o");
    let pad_names = write_probe_spacer_object(&pad_obj, 8, 4, 4096).expect("write spacer.o");
    assert_eq!(pad_names.len(), 12);
    let parsed_pad = build_order_list(std::slice::from_ref(&pad_obj));
    assert_eq!(parsed_pad.len(), 12);

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        let sdk = aterm_uds::spawnfd::command(Path::new("/usr/bin/xcrun"))
            .args(["--show-sdk-path"])
            .output()
            .expect("xcrun --show-sdk-path");
        let sdk_path = String::from_utf8_lossy(&sdk.stdout).trim().to_string();
        let dylib_out = scratch.join("probe_test.dylib");
        let order_list = vec![
            "__atpad_t_00003".to_string(),
            "__atpad_t_00000".to_string(),
            "__atpad_c_00001".to_string(),
        ];
        let pad_order = scratch.join("pad.order");
        write_order_file(&pad_order, &order_list).expect("write pad.order");
        let out = aterm_uds::spawnfd::command(Path::new("/usr/bin/xcrun"))
            .args([
                "ld",
                "-dylib",
                "-arch",
                "arm64",
                "-platform_version",
                "macos",
                "14.0.0",
                "14.0.0",
                "-syslibroot",
                &sdk_path,
                "-lSystem",
                "-dead_strip",
                "-o",
            ])
            .arg(&dylib_out)
            .arg(&pad_obj)
            .arg("-order_file")
            .arg(&pad_order)
            .output()
            .expect("invoke xcrun ld");
        assert!(
            out.status.success(),
            "ld failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let nm_out = aterm_uds::spawnfd::command(Path::new("/usr/bin/nm"))
            .arg("-n")
            .arg(&dylib_out)
            .output()
            .expect("nm dylib");
        let nm_text = String::from_utf8_lossy(&nm_out.stdout);
        let pos_3 = nm_text
            .find("__atpad_t_00003")
            .expect("spacer 3 kept under -dead_strip");
        let pos_0 = nm_text
            .find("__atpad_t_00000")
            .expect("spacer 0 kept under -dead_strip");
        assert!(
            pos_3 < pos_0,
            "-order_file must place __atpad_t_00003 before __atpad_t_00000:\n{nm_text}"
        );
    }
}

/// A macOS release image's final link takes the permutation sieve's order or none. Where the
/// sieve gave no order, or an empty one, the link's arguments are its own then `-x`, with no
/// `-order_file` though its inputs hold symbols the structural order lays out, and the wrapper
/// writes no order. Where the sieve gave one, the link names the order file once, ahead of
/// `-x`, and the wrapper writes exactly the sieve's order to it. A link that strips local
/// symbols itself, in its arguments or in a response file, takes no second `-x`. Fewer than
/// [`MIN_PROBE_HIT_SETS`] hit sets refine no order, where that many refine one.
#[test]
fn a_final_link_takes_the_sieves_order_or_none() {
    let scratch_dir = aterm_tempfile::Builder::new()
        .prefix("aterm-final-link-test-")
        .suffix(".noindex")
        .tempdir()
        .expect("create scratch dir");
    let scratch = scratch_dir.path();
    let obj = scratch.join("alpha.o");
    std::fs::write(
        &obj,
        build_minimal_macho64(
            MH_OBJECT,
            0,
            64,
            &[("__RNvNtCs1_3agh3cli3run", 1, 0x0), ("_main", 1, 0x10)],
        ),
    )
    .expect("write alpha.o");
    let shown = |path: &Path| path.display().to_string();
    let link_args = vec!["-o".to_owned(), shown(&scratch.join("aterm")), shown(&obj)];
    let expanded = expand_linker_args(&link_args);
    let structural = build_order_list(&collect_input_paths(&expanded));
    assert_eq!(
        structural,
        ["_main", "__RNvNtCs1_3agh3cli3run"],
        "the link's inputs have a structural order"
    );
    let order_file = scratch.join("aterm.order");
    let own_then = |args: &[String], tail: &[&std::ffi::OsStr]| -> Vec<OsString> {
        args.iter()
            .map(OsString::from)
            .chain(tail.iter().map(|arg| arg.to_os_string()))
            .collect()
    };
    let strip: &std::ffi::OsStr = "-x".as_ref();

    let unrefined = final_link(&link_args, &expanded, None, &order_file);
    assert_eq!(
        unrefined,
        FinalLink {
            args: own_then(&link_args, &[strip]),
            order: None,
        }
    );
    assert_eq!(
        final_link(&link_args, &expanded, Some(Vec::new()), &order_file),
        unrefined
    );

    let sieve = vec!["__RNvNtCs1_3agh3cli3run".to_owned(), "_main".to_owned()];
    assert_eq!(
        final_link(&link_args, &expanded, Some(sieve.clone()), &order_file),
        FinalLink {
            args: own_then(
                &link_args,
                &["-order_file".as_ref(), order_file.as_os_str(), strip]
            ),
            order: Some(sieve),
        }
    );

    let stripped = [link_args.clone(), vec!["-x".to_owned()]].concat();
    assert_eq!(
        final_link(&stripped, &expand_linker_args(&stripped), None, &order_file).args,
        own_then(&stripped, &[])
    );
    let response = scratch.join("strip.args");
    std::fs::write(&response, "-x\n").expect("write response file");
    let via_response = [link_args.clone(), vec![format!("@{}", response.display())]].concat();
    assert_eq!(
        final_link(
            &via_response,
            &expand_linker_args(&via_response),
            None,
            &order_file
        )
        .args,
        own_then(&via_response, &[])
    );

    let hot: HashSet<String> = ["_main".to_owned()].into_iter().collect();
    assert_eq!(
        refine_order_with_probe_hits(&structural, &vec![hot.clone(); MIN_PROBE_HIT_SETS - 1]),
        None
    );
    assert_eq!(
        refine_order_with_probe_hits(&structural, &vec![hot; MIN_PROBE_HIT_SETS]),
        Some(structural)
    );
}

/// Where `world` falls short of a scratch world (`aterm_types::world`), each as a name: the
/// `aterm.toml` its image reads (`aterm/aterm.toml` under its `XDG_CONFIG_HOME`) where the world
/// writes none; each root key of [`PROBE_ROOT_KEYS`] that file does not hold at its root with the
/// probe's value; each `table.key` of [`aterm_types::world::LANES_OFF`] it does not hold with the
/// same value; and [`aterm_types::world::MARKER`] where the environment does not carry it at the
/// state folder under its HOME ([`aterm_types::world::state_home`]).
fn short_of_a_world(world: &ProbeWorld) -> Vec<String> {
    let var = |name: &str| {
        world
            .env
            .iter()
            .find(|(held, _)| *held == name)
            .map(|(_, value)| PathBuf::from(value))
    };
    let mut short = Vec::new();
    let read = var("XDG_CONFIG_HOME").map(|config| config.join("aterm/aterm.toml"));
    let file = world
        .files
        .iter()
        .find(|(path, _)| Some(path) == read.as_ref());
    let text = match file {
        Some((_, text)) => text.as_str(),
        None => {
            short.push("aterm.toml".to_owned());
            ""
        }
    };
    let doc = aterm_toml::from_str::<aterm_toml::Value>(text)
        .expect("the image's aterm.toml is one document");
    let own = aterm_toml::from_str::<aterm_toml::Value>(PROBE_ROOT_KEYS)
        .expect("the probe's root keys are one document");
    for (key, value) in own.as_table().expect("a document is a table") {
        if doc.get(key) != Some(value) {
            short.push(key.clone());
        }
    }
    let off = aterm_toml::from_str::<aterm_toml::Value>(aterm_types::world::LANES_OFF)
        .expect("the lanes-off tables are one document");
    for (table, keys) in off.as_table().expect("a document is a table") {
        for (key, value) in keys.as_table().expect("each entry is a table") {
            if doc.get(table).and_then(|held| held.get(key)) != Some(value) {
                short.push(format!("{table}.{key}"));
            }
        }
    }
    let state = var("HOME").and_then(|home| aterm_types::world::state_home(&home));
    if state.is_none() || var(aterm_types::world::MARKER) != state {
        short.push(aterm_types::world::MARKER.to_owned());
    }
    short
}

/// A macOS release link's layout probe starts each image it measures, a release aterm under a
/// fresh HOME, in a scratch world (`aterm_types::world`), as verify measure, the GUI smoke and the
/// perf arena do: the `aterm.toml` the image reads holds the probe's root keys at its root and,
/// word for word after them, the lanes-off tables aterm-types holds; every process the probe
/// starts in the world begins from an environment that carries the world's marker at the state
/// folder under its HOME; each file it writes sits in a folder it makes first; and everything it
/// lays out sits under the root it removes. On a host that is no platform a world is laid out
/// for there is no world, and the probe starts no image.
#[test]
fn a_layout_probe_starts_its_images_in_a_scratch_world() {
    let root = Path::new("/w/atord");
    let Some(world) = probe_world(root) else {
        assert_eq!(
            aterm_types::world::state_home(&root.join("home")),
            None,
            "a platform with state under HOME has a world"
        );
        return;
    };
    assert_eq!(short_of_a_world(&world), Vec::<String>::new());
    let config = world
        .files
        .iter()
        .find(|(path, _)| path.ends_with("aterm/aterm.toml"))
        .map(|(_, text)| text.as_str())
        .unwrap_or_default();
    assert_eq!(
        config.strip_prefix(PROBE_ROOT_KEYS),
        Some(aterm_types::world::LANES_OFF),
        "{config}"
    );
    let home = world
        .env
        .iter()
        .find(|(name, _)| *name == "HOME")
        .map(|(_, value)| PathBuf::from(value));
    assert_eq!(home.as_ref(), Some(&world.home));
    assert!(world.dirs.contains(&world.home));
    for (path, _) in &world.files {
        let folder = path.parent().map(Path::to_path_buf);
        assert!(
            folder.is_some_and(|folder| world.dirs.contains(&folder)),
            "{} is written in a folder the probe does not make",
            path.display()
        );
    }
    let written = world.files.iter().map(|(path, _)| path);
    for path in world.dirs.iter().chain(written) {
        assert!(
            path.starts_with(root),
            "{} lies outside the probe's root",
            path.display()
        );
    }
}

/// NEGATIVE CONTROL: the probe's world before it took the scratch world's words (its own copy of
/// three of the five lanes-off tables, no `[keeper]` or `[swarm]`, and no marker) falls short by
/// exactly those, and one whose root keys follow the tables, which TOML reads into the last
/// table, falls short by its root keys. Each case takes the probe's layout with its own
/// `aterm.toml` and sets or leaves out the marker itself, so neither leans on the probe's own.
#[cfg(unix)]
#[test]
fn a_probe_world_without_the_scratch_worlds_words_falls_short() {
    let world = probe_world(Path::new("/w/atord")).expect("Unix keeps state under HOME");
    let state = aterm_types::world::state_home(&world.home).expect("Unix keeps state under HOME");
    let with = |config: &str, marker: bool| ProbeWorld {
        home: world.home.clone(),
        dirs: world.dirs.clone(),
        files: world
            .files
            .iter()
            .map(|(path, text)| {
                let text = if path.ends_with("aterm/aterm.toml") {
                    config
                } else {
                    text.as_str()
                };
                (path.clone(), text.to_owned())
            })
            .collect(),
        env: world
            .env
            .iter()
            .filter(|(name, _)| *name != aterm_types::world::MARKER)
            .cloned()
            .chain(marker.then(|| (aterm_types::world::MARKER, state.clone().into_os_string())))
            .collect(),
    };
    let before = "bell_sound = false\ntemporal_recording = true\n\
                  [update]\nenabled = false\nauto_apply = false\n\
                  [packages]\nenabled = false\n\
                  [machine]\nuniversal_control = \"leave\"\n";
    assert_eq!(
        short_of_a_world(&with(before, false)),
        [
            "keeper.enabled",
            "swarm.enabled",
            aterm_types::world::MARKER,
        ]
    );
    let tables_first = format!("{}{PROBE_ROOT_KEYS}", aterm_types::world::LANES_OFF);
    assert_eq!(
        short_of_a_world(&with(tables_first.as_str(), true)),
        ["bell_sound", "temporal_recording"]
    );
}
