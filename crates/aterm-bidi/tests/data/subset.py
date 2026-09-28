#!/usr/bin/env python3
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
"""Cut the checked-in conformance subsets from the full UCD test files.

    python3 subset.py BidiTest.txt BidiCharacterTest.txt

BidiCharacterTest: every line of the hand-written sections (everything before
the generated "Permutations of sequences containing paired brackets" block)
plus every 40th line after it. BidiTest: its header, every @Levels/@Reorder
line (each data line is judged against the most recent ones), and every 150th
data line. The full files pass too (91,707 and 770,241 cases, measured
2026-09-25); the subset keeps the repository small.
"""

import sys


def cut_character_test(src, dst):
    lines = open(src, encoding="utf-8").read().splitlines()
    marker = next(i for i, l in enumerate(lines) if "Permutations of sequences" in l)
    out = lines[:marker]
    data = [l for l in lines[marker:] if l and not l.startswith("#")]
    out.append("# Generated permutation cases: every 40th line (subset.py).")
    out.extend(data[::40])
    open(dst, "w", encoding="utf-8").write("\n".join(out) + "\n")


def cut_bidi_test(src, dst):
    out, seen = [], 0
    for line in open(src, encoding="utf-8").read().splitlines():
        if not line or line.startswith("#"):
            if not seen:
                out.append(line)
            continue
        if line.startswith("@"):
            out.append(line)
            continue
        if seen % 150 == 0:
            out.append(line)
        seen += 1
    open(dst, "w", encoding="utf-8").write("\n".join(out) + "\n")


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    cut_bidi_test(sys.argv[1], "BidiTest-subset.txt")
    cut_character_test(sys.argv[2], "BidiCharacterTest-subset.txt")
