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
    );
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
