// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

#[test]
fn preserves_variables_and_flag_lists_but_protects_path_words() {
    let source = b"prefix=@@HOMEBREW_PREFIX@@\nlibdir=${prefix}/lib\nName: ${name}\nLibs: -L${libdir} -lzstd ${extra_libs}\nCflags: -I ${prefix}/include -F\"${frameworks}\" -DNAME=value\n";
    let actual = prepare(Path::new("libzstd.pc"), source.to_vec());
    assert_eq!(
        String::from_utf8(actual).unwrap(),
        "prefix=@@HOMEBREW_PREFIX@@\nlibdir=${prefix}/lib\nName: ${name}\nLibs: \"-L${libdir}\" -lzstd ${extra_libs}\nCflags: -I \"${prefix}/include\" -F\"${frameworks}\" -DNAME=value\n"
    );
    assert_eq!(prepare(Path::new("other.conf"), source.to_vec()), source);
}
