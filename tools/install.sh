#!/usr/bin/env bash
# Copyright 2026 Andrew Yates
# SPDX-License-Identifier: Apache-2.0
#
# install.sh — batteries-included aterm install: the released aterm.app AND the
# `aterm` command (ONE name on PATH; it fronts every verb — aterm help / ctl /
# pkg / fleet / drive), in one command. Flags only EXCLUDE.
#
# The DEFAULT download source is the PUBLIC release repo (alabsystems/aterm),
# fetched anonymously — no GitHub credential required. An authenticated `gh`
# is preferred when present (it serves any slug), and is REQUIRED for the
# private staging repo: ATERM_REPO_SLUG=alabsystems/aterm, or a run from the
# private checkout, whose Cargo.toml derives that slug. When a gh credential
# was used, the script COPIES it into the updater's own 0600 store (the
# `token` half below), so the installed copy keeps itself up to date without
# `gh` on PATH — which a Finder-launched .app does not have.
#
# The three halves, and when each can run:
#   app — the released aterm.app from the GitHub Release (macOS only; anonymous
#         curl against a public repo, authenticated gh otherwise).
#         Verified with a deliberately weaker bootstrap tier than the installed
#         updater (see docs/RELEASING.md):
#           1. paginate the complete release catalog and select the unique
#              numeric maximum of its current-scheme vMAJOR.MINOR.PATCH tags,
#              independent of GitHub REST row order (NOT the "latest" pointer,
#              which non-app releases can hold). Retired two-component tags are
#              archive history: they are skipped, never elected, exactly as the
#              in-app updater skips them. An explicit --version pin bypasses
#              selection and may still name an archived release
#           2. require exactly one manifest and canonical DMG asset, carrying
#              each exact API asset ID and byte size into its download
#           3. bind tag == manifest version == DMG filename, then verify the
#              DMG's SHA-256 against that manifest
#           4. verify the bundle's code signature; when a Team ID is pinned
#              (manifest team_id, or $ATERM_TEAM_ID), require the Developer-ID
#              requirement chain for that team + notarization
#           5. swap aterm.app into /Applications — or ~/Applications when
#              /Applications isn't user-writable — replacing any existing copy
#         BOOTSTRAP TRUST BOUNDARY: this script does not verify the manifest's
#         Ed25519 signature. It trusts the transport's repo metadata — a
#         gh-authenticated API session, or TLS to api.github.com on the
#         anonymous lane — plus the manifest digest (Tier REPO). Releases are
#         unsigned by default and carry no .sig asset; update signing is
#         optional, so the installed updater only demands a signature if a key
#         was compiled in.
#   cli — the `aterm` command (transparent PTY passthrough + the front door
#         for every verb). ONE name lands on PATH — the [workspace.metadata.
#         atpkg] expose declaration; the verb siblings (aterm-ctl, atpkg,
#         aterm-fleet, aterm-drive) ride CO-LOCATED and `aterm ctl/pkg/fleet/
#         drive` resolve them via current_exe, never PATH. Preferred source:
#         the installed app bundle — newer releases ship the whole toolset
#         inside Contents/MacOS (the CLI terminal under the name `aterm-cli`;
#         the bundle's `aterm` is the GUI) — so this half is ONE SYMLINK into
#         the bundle: no cargo, works from a piped script, and the link tracks
#         in-place app updates. Fallback: the toolset is built from THIS
#         checkout's source (run `git pull --ff-only` first for the latest
#         main; needs a checkout + cargo + the pinned CUSTOM `trust` toolchain
#         — rust-toolchain.toml; rustup cannot download it, so its absence is
#         a loud pre-flight skip naming the acquisition path, never a
#         mid-build abort) into a private store
#         (~/.local/lib/aterm/bin, override ATERM_STORE_DIR) with the one
#         `aterm` symlink in ~/.local/bin — non-macOS, older bundles, or no
#         installed app. A PATH hint prints if ~/.local/bin isn't on PATH.
#   token — a per-machine GitHub token for the IN-APP UPDATER, written to
#         "~/Library/Application Support/aterm/update-token" (0600, in a 0700
#         dir). Sourced from $ATERM_UPDATE_TOKEN if set, else `gh auth token`.
#         NOT needed for the default channel: the compiled-in update source is
#         the PUBLIC repo, which the updater reads anonymously, and for it the
#         token chain consults only an explicit $ATERM_UPDATE_TOKEN — never the
#         keychain and never this file (crates/aterm-update-core/src/token.rs,
#         `needs_ambient_credential`). The file matters when a machine points
#         the updater somewhere else with $ATERM_UPDATE_OWNER/_REPO: only then
#         does the chain read it, and only then can the source be a repo that
#         requires authentication. Provisioning it here keeps that case working
#         without `gh` on PATH (a Finder-launched .app has a minimal PATH).
#         On the public channel a token only buys the faster check cadence
#         (~75s vs ~15min), and ONLY via an exported $ATERM_UPDATE_TOKEN — this
#         file cannot supply it there.
#         macOS only (the updater is macOS-only); idempotent; the token is
#         never printed; --no-token skips it. Re-running refreshes a rotated
#         token, and no failure here is fatal: the app is installed and, on the
#         public channel, updating regardless.
#
# FAILSAFE POLICY: each half is pre-flighted BEFORE any install work; a half
# that is impossible in this environment (piped script with no checkout,
# non-macOS, a repo needing credentials this run lacks, missing cargo or the
# pinned trust toolchain, no release yet, unwritable destination) is
# SKIPPED with a loud reason and the rest still installs. Exit 1 when nothing
# was installed. A real mid-flight failure (download, SHA-256 / signature
# verification, build) always aborts non-zero — those are never skipped.
# --version selects the app release only — the symlinked `aterm` follows
# whatever app is installed, and the cargo fallback always builds the checkout.
#
# Usage:
#   tools/install.sh                                  # everything (the default)
#   tools/install.sh --no-cli                         # exclude the `aterm` command
#   tools/install.sh --no-app                         # exclude the app
#   tools/install.sh --no-token                       # don't provision the update token
#   tools/install.sh --no-app --no-cli                # ONLY provision the token — for a
#                                                     # machine that points the updater at
#                                                     # a private repo (see `token` above)
#   tools/install.sh --version 0.5.0                  # pin the app release
#   tools/install.sh --uninstall                      # reverse everything it installed
#   tools/install.sh --uninstall --dry-run            # ...show what that would remove
#   ATERM_REPO_SLUG=my-org/aterm tools/install.sh     # fork / relocated repo
#   ATERM_INSTALL_DIR="$HOME/Applications" tools/install.sh
#   ATERM_BIN_DIR=/usr/local/bin tools/install.sh     # the `aterm` symlink elsewhere
#   ATERM_STORE_DIR=/opt/aterm tools/install.sh       # source-built toolset elsewhere
#   ATERM_TEAM_ID=ABCDE12345 tools/install.sh         # require this signing team
#
# One-liner, no clone needed (the cli half symlinks from the just-installed
# app when its bundle ships the tools; against older releases the cli half is
# skipped with a note, since building from source needs a checkout):
#   curl -fsSL https://raw.githubusercontent.com/alabsystems/aterm/HEAD/tools/install.sh | bash
# Operators installing from the PRIVATE staging repo (needs authenticated gh):
#   gh api -H "Accept: application/vnd.github.raw" \
#     repos/alabsystems/aterm/contents/tools/install.sh |
#     ATERM_REPO_SLUG=alabsystems/aterm bash
set -euo pipefail

# Is this file really on disk (vs piped/`bash -s`, where BASH_SOURCE is unusable —
# and on bash >= 5 points at the bash BINARY itself, so -r alone is not enough)?
self_on_disk() {
	[[ -r "${BASH_SOURCE[0]:-}" && "${BASH_SOURCE[0]}" == *install.sh ]]
}

usage() {
	if self_on_disk; then
		# Print the header comment: from line 5 to the first non-comment line, drop it.
		sed -n '5,/^[^#]/p' "${BASH_SOURCE[0]}" | sed '$d' | sed 's/^# \{0,1\}//'
	else
		echo "usage: install.sh [--no-cli] [--no-app] [--no-token] [--version X.Y.Z] [--uninstall [--dry-run]]   (env: ATERM_REPO_SLUG, ATERM_INSTALL_DIR, ATERM_BIN_DIR, ATERM_STORE_DIR, ATERM_MAN_DIR, ATERM_TEAM_ID, ATERM_UPDATE_TOKEN)"
	fi
}

# --- pure update-channel arbitration (shared with deterministic shell tests) ---

# Match the updater's u64 component domain without asking Bash 3.2 to perform
# signed-machine-word arithmetic. Components reach this already canonical (no
# leading zeroes), so equal-length decimal strings compare lexically == numerically.
numeric_component_in_u64() {
	local component="$1"
	local LC_ALL=C
	[[ "$component" =~ ^(0|[1-9][0-9]*)$ ]] || return 1
	[[ "${#component}" -lt 20 ]] ||
		[[ "${#component}" -eq 20 && ( "$component" == 18446744073709551615 || "$component" < 18446744073709551615 ) ]]
}

# Compare a canonical unsigned decimal string against small canonical bounds
# without signed arithmetic or octal interpretation.
decimal_in_closed_range() {
	local value="$1" minimum="$2" maximum="$3"
	local LC_ALL=C
	[[ "$value" =~ ^(0|[1-9][0-9]*)$ ]] || return 1
	if [[ "${#value}" -lt "${#minimum}" ]] ||
		[[ "${#value}" -eq "${#minimum}" && "$value" < "$minimum" ]]; then
		return 1
	fi
	if [[ "${#value}" -gt "${#maximum}" ]] ||
		[[ "${#value}" -eq "${#maximum}" && "$value" > "$maximum" ]]; then
		return 1
	fi
}

# Classify one published release tag, mirroring the updater's parse_numeric_tag
# (crates/aterm-update/src/github.rs). Sets TAG_KIND_RESULT to:
#   candidate — the current scheme: canonical vMAJOR.MINOR.PATCH, i.e. the
#               workspace MAJOR.MINOR.DEV version with DEV reset to 0
#               (VERSIONING.md). Only these are ever installed by default.
#   legacy    — a retired pre-cut-over two-component vMAJOR.MINOR. Those
#               releases stay published as archive history and are skipped, not
#               errors, so the archive can coexist with the current channel.
# Everything else fails closed (returns 1): no v prefix, fewer than two or more
# than three components, an empty or non-numeric component, a leading-zero
# spelling, or a component outside the updater's u64 domain. Garbage in the tag
# namespace must stop the check, never silently narrow the candidate set.
parse_release_tag() {
	local tag="$1" body part
	# ASCII digits only, in every locale — the updater matches is_ascii_digit.
	local LC_ALL=C
	local -a parts
	TAG_KIND_RESULT=""
	[[ "$tag" == v* ]] || return 1
	body="${tag#v}"
	# `read -a` discards a final empty field on Bash 3.2, so validate the whole
	# grammar first; this rejects both v0.55. and v0..55 before component parsing.
	[[ "$body" =~ ^[0-9]+(\.[0-9]+)+$ ]] || return 1
	IFS='.' read -r -a parts <<<"$body"
	for part in "${parts[@]}"; do
		# Reject leading-zero spellings so a tag has exactly ONE canonical form
		# and two distinct tags can never share a numeric order. The u64 bound is
		# checked BEFORE the arity split, exactly as the updater does: an
		# overflowing two-component tag is malformed, not a tolerated legacy row.
		[[ "${#part}" -eq 1 || "${part:0:1}" != 0 ]] || return 1
		numeric_component_in_u64 "$part" || return 1
	done
	case "${#parts[@]}" in
	3) TAG_KIND_RESULT=candidate ;;
	2) TAG_KIND_RESULT=legacy ;;
	*) return 1 ;;
	esac
}

# A canonical numeric dotted tag of either arity. This is the shape gate for
# operator input and for the manifest identity bind — an explicit --version may
# still name a retired two-component archive release — and for the min_os
# advisory, whose operands are macOS product versions, not aterm tags.
canonical_numeric_tag() {
	parse_release_tag "$1"
}

# The authority contract: the release the channel elects is spelled exactly
# vMAJOR.MINOR.PATCH. A retired two-component release can never hold this
# position — the same refusal as the updater's canonical_authority_version.
canonical_authority_tag() {
	parse_release_tag "$1" && [[ "$TAG_KIND_RESULT" == candidate ]]
}

# Set TAG_COMPARE_RESULT to -1, 0, or 1 using decimal-string component
# comparison: shorter canonical decimal first, then lexical within a width, so
# 9 < 10 and 99 < 100 order numerically. macOS still ships Bash 3.2, so this
# deliberately avoids namerefs, associative arrays, and signed-machine-word
# arithmetic. Both operands must already be canonical (parse_release_tag), which
# is what makes "same length ⇒ lexical == numeric" hold.
compare_numeric_tags() {
	local lhs="$1" rhs="$2" body i common l r
	local LC_ALL=C
	local -a left right
	parse_release_tag "$lhs" && parse_release_tag "$rhs" || return 2
	body="${lhs#v}"
	IFS='.' read -r -a left <<<"$body"
	body="${rhs#v}"
	IFS='.' read -r -a right <<<"$body"
	common="${#left[@]}"
	[[ "${#right[@]}" -lt "$common" ]] && common="${#right[@]}"
	for ((i = 0; i < common; i++)); do
		l="${left[$i]}"
		r="${right[$i]}"
		if [[ "${#l}" -lt "${#r}" ]]; then TAG_COMPARE_RESULT=-1; return 0; fi
		if [[ "${#l}" -gt "${#r}" ]]; then TAG_COMPARE_RESULT=1; return 0; fi
		if [[ "$l" < "$r" ]]; then TAG_COMPARE_RESULT=-1; return 0; fi
		if [[ "$l" > "$r" ]]; then TAG_COMPARE_RESULT=1; return 0; fi
	done
	if [[ "${#left[@]}" -lt "${#right[@]}" ]]; then
		TAG_COMPARE_RESULT=-1
	elif [[ "${#left[@]}" -gt "${#right[@]}" ]]; then
		TAG_COMPARE_RESULT=1
	else
		TAG_COMPARE_RESULT=0
	fi
}

# Input rows are TAG<TAB>DRAFT<TAB>EXACT_MANIFEST_COUNT, emitted for every page
# by gh's embedded jq. Output is the unique numeric maximum of the CURRENT-scheme
# vMAJOR.MINOR.PATCH candidates; retired two-component releases are skipped even
# when their numbers are larger (v0.61 does not outrank v0.5.0 — it is not in the
# running at all). Return 1 for no app candidate and 2 for malformed/ambiguous/
# noncanonical authority.
select_authoritative_tag() {
	local rows="$1" tag draft manifest_count extra selected=""
	while IFS=$'\t' read -r tag draft manifest_count extra ||
		[[ -n "${tag}${draft}${manifest_count}${extra}" ]]; do
		[[ -n "${tag}${draft}${manifest_count}${extra}" ]] || continue
		if [[ -n "$extra" || ( "$draft" != true && "$draft" != false ) ||
			! "$manifest_count" =~ ^[0-9]+$ ]]; then
			echo "install.sh: malformed release metadata row for ${tag:-<missing-tag>}" >&2
			return 2
		fi
		[[ "$draft" == true ]] && continue
		[[ "$manifest_count" == 0 ]] && continue
		if [[ "$manifest_count" != 1 ]]; then
			echo "install.sh: release $tag has duplicate aterm-appcast.toml assets" >&2
			return 2
		fi
		if ! parse_release_tag "$tag"; then
			echo "install.sh: app release tag $tag is not numeric dotted vN.N.N" >&2
			return 2
		fi
		# Retired-scheme releases stay published but are never installed. Skipping
		# (rather than erroring) is what lets the pre-cut-over archive coexist with
		# the current channel — the same `continue` the updater's selector takes.
		[[ "$TAG_KIND_RESULT" == candidate ]] || continue
		if [[ -z "$selected" ]]; then
			selected="$tag"
		else
			compare_numeric_tags "$tag" "$selected" || return 2
			case "$TAG_COMPARE_RESULT" in
			1) selected="$tag" ;;
			0)
				echo "install.sh: published app releases $selected and $tag have the same numeric order" >&2
				return 2
				;;
			esac
		fi
	done <<<"$rows"
	[[ -n "$selected" ]] || return 1
	if ! canonical_authority_tag "$selected"; then
		echo "install.sh: authoritative app release $selected is not canonical vMAJOR.MINOR.PATCH" >&2
		return 2
	fi
	printf '%s\n' "$selected"
}

# Require one exact API asset ID. This is separately tested because using an
# order-dependent first match would make duplicate assets an availability bug.
require_unique_asset_id() {
	local ids="$1" label="$2" id count=0 selected=""
	while IFS= read -r id || [[ -n "$id" ]]; do
		[[ -n "$id" ]] || continue
		if [[ ! "$id" =~ ^[1-9][0-9]*$ ]]; then
			echo "install.sh: malformed asset ID for $label" >&2
			return 2
		fi
		count=$((count + 1))
		selected="$id"
	done <<<"$ids"
	if [[ "$count" -ne 1 ]]; then
		echo "install.sh: release has $count assets named $label; expected exactly one" >&2
		return 2
	fi
	printf '%s\n' "$selected"
}

# Bind tag, manifest version, local path shape, and digest before any DMG lookup,
# join, or download. The elected authority is always vMAJOR.MINOR.PATCH; an
# explicit --version may also name a retired two-component archive release, and
# either way the tag, the manifest spelling, and aterm-<version>.dmg must agree
# exactly — the same identity bind the updater enforces.
validate_manifest_identity() {
	local tag="$1" version="$2" dmg_name="$3" sha256="$4"
	if ! canonical_numeric_tag "$tag" || [[ "$tag" != "v$version" ]]; then
		echo "install.sh: manifest version $version does not exactly match release tag $tag" >&2
		return 2
	fi
	if [[ "$dmg_name" != "aterm-$version.dmg" ]]; then
		echo "install.sh: manifest DMG $dmg_name is not canonical aterm-$version.dmg" >&2
		return 2
	fi
	if [[ ! "$sha256" =~ ^[0-9a-fA-F]{64}$ ]]; then
		echo "install.sh: manifest sha256 is not exactly 64 hexadecimal digits" >&2
		return 2
	fi
}

# --- anonymous-lane extractors: the gh-embedded jq's job, without gh ----------
#
# The anonymous lane reads api.github.com directly, which pretty-prints: in the
# release LIST response, release-object fields sit at exactly 4 spaces and asset
# fields at exactly 8; in the single-release (tags/<tag>) response, asset objects
# open at exactly 4 and their fields sit at exactly 6. JSON strings cannot carry
# a raw newline or raw tab, so exact-indent anchoring cannot be forged from a
# release name or body, and the emitted TSV cannot be split by a value. This is
# NOT a permissive JSON parser: any release/asset whose expected fields arrive
# in an unexpected shape emits a deliberately malformed row, which the row
# validators downstream (select_authoritative_tag / require_unique_asset_record)
# refuse LOUDLY — a parser surprise stops the install, never narrows the set.

# stdin: one page of /repos/<slug>/releases JSON.
# stdout: TAG<TAB>DRAFT<TAB>EXACT_MANIFEST_COUNT per release — the same rows the
# gh lane's embedded jq emits for select_authoritative_tag.
anon_release_rows() {
	awk '
		/^  \{$/ { open = 1; bad = 0; tag = ""; draft = ""; count = 0; next }
		open != 1 { next }
		/^    "tag_name":/ {
			if ($0 ~ /^    "tag_name": "[^"]*",?$/) {
				tag = $0
				sub(/^    "tag_name": "/, "", tag)
				sub(/",?$/, "", tag)
			} else bad = 1
		}
		/^    "draft":/ {
			if ($0 ~ /^    "draft": (true|false),?$/) {
				draft = $0
				sub(/^    "draft": /, "", draft)
				sub(/,$/, "", draft)
			} else bad = 1
		}
		/^        "name": "aterm-appcast\.toml",?$/ { count += 1 }
		/^  \},?$/ {
			if (bad || tag == "" || draft == "") print "MALFORMED\tMALFORMED\tMALFORMED\tMALFORMED"
			else print tag "\t" draft "\t" count
			open = 0
		}
	'
}

# stdin: one /repos/<slug>/releases/tags/<tag> JSON document.
# stdout: ID<TAB>SIZE for every asset whose name is exactly $1 — the same rows
# the gh lane's embedded jq feeds require_unique_asset_record.
anon_asset_records() {
	awk -v wanted="$1" '
		/^    \{$/ { open = 1; bad = 0; n = ""; id = ""; size = ""; next }
		open != 1 { next }
		/^      "name":/ {
			if ($0 ~ /^      "name": "[^"]*",?$/) {
				n = $0
				sub(/^      "name": "/, "", n)
				sub(/",?$/, "", n)
			} else bad = 1
		}
		/^      "id":/ {
			if ($0 ~ /^      "id": [0-9]+,?$/) {
				id = $0
				sub(/^      "id": /, "", id)
				sub(/,$/, "", id)
			} else bad = 1
		}
		/^      "size":/ {
			if ($0 ~ /^      "size": [0-9]+,?$/) {
				size = $0
				sub(/^      "size": /, "", size)
				sub(/,$/, "", size)
			} else bad = 1
		}
		/^    \},?$/ {
			if (n == wanted) {
				if (bad || id == "" || size == "") print "MALFORMED\tMALFORMED\tMALFORMED"
				else print id "\t" size
			}
			open = 0
		}
	'
}

# Return every exact ID/size record for a validated tag/name pair. Callers must
# apply require_unique_asset_record before using the result; keeping immutable
# API identity metadata makes the subsequent octet download order-independent.
# Both lanes carry the same records, so every uniqueness/bounds gate downstream
# is lane-independent. APP_LANE defaults to gh so the library-only test seam
# (which mocks `gh`) keeps its transport semantics.
release_asset_records() {
	local tag="$1" name="$2"
	canonical_numeric_tag "$tag" || {
		echo "install.sh: refusing asset lookup for invalid release tag $tag" >&2
		return 2
	}
	case "$name" in
	aterm-appcast.toml | aterm-appcast.toml.sig) ;;
	*)
		# Two canonical container names, both anchored and version-shaped. The zip
		# is admitted because Intel Macs install FROM it: no ALab artifact is
		# published for x86_64, so those machines take the lean container instead
		# of ~600 MB of aarch64 payload they cannot use. Without this arm the whole
		# lean lane was dead on arrival — every Intel install aborted here with
		# "noncanonical name" before downloading a byte.
		#
		# Kept as an explicit allowlist rather than a loosened pattern: the point of
		# this gate is that a manifest cannot name an arbitrary asset in the
		# release, and `-mac.zip` is exactly as constrained as `.dmg`.
		[[ "$name" =~ ^aterm-[0-9]+(\.[0-9]+)+\.dmg$ ||
			"$name" =~ ^aterm-[0-9]+(\.[0-9]+)+-mac\.zip$ ]] || {
			echo "install.sh: refusing asset lookup for noncanonical name $name" >&2
			return 2
		}
		;;
	esac
	if [[ "${APP_LANE:-gh}" == gh ]]; then
		gh api "repos/$REPO_SLUG/releases/tags/$tag" \
			--jq ".assets[] | select(.name == \"$name\") | [(.id | tostring), (.size | tostring)] | @tsv"
	else
		curl -fsS -H "Accept: application/vnd.github+json" \
			"https://api.github.com/repos/$REPO_SLUG/releases/tags/$tag" |
			anon_asset_records "$name"
	fi
}

require_unique_asset_record() {
	local records="$1" label="$2" minimum="$3" maximum="$4"
	local id size extra count=0 selected_id="" selected_size=""
	while IFS=$'\t' read -r id size extra || [[ -n "${id}${size}${extra}" ]]; do
		[[ -n "${id}${size}${extra}" ]] || continue
		if [[ -n "$extra" || ! "$id" =~ ^[1-9][0-9]*$ ]] ||
			! decimal_in_closed_range "$size" "$minimum" "$maximum"; then
			echo "install.sh: malformed or out-of-bounds asset metadata for $label" >&2
			return 2
		fi
		count=$((count + 1))
		selected_id="$id"
		selected_size="$size"
	done <<<"$records"
	if [[ "$count" -ne 1 ]]; then
		echo "install.sh: release has $count assets named $label; expected exactly one" >&2
		return 2
	fi
	printf '%s\t%s\n' "$selected_id" "$selected_size"
}

release_unique_asset_record() {
	local tag="$1" name="$2" minimum="$3" maximum="$4" records
	if ! records="$(release_asset_records "$tag" "$name")"; then
		echo "install.sh: could not resolve $name in release $tag" >&2
		return 2
	fi
	require_unique_asset_record "$records" "$name" "$minimum" "$maximum"
}

# The one producer of asset octets, per lane. Both stream the exact immutable
# asset ID (never a name or "latest" pointer), so the identity carried from
# release_asset_records is what gets downloaded on either transport.
fetch_asset_octets() {
	if [[ "${APP_LANE:-gh}" == gh ]]; then
		gh api -H "Accept: application/octet-stream" "repos/$REPO_SLUG/releases/assets/$1"
	else
		# -L: the API answers an octet-stream request with a redirect to the CDN.
		curl -fsSL -H "Accept: application/octet-stream" \
			"https://api.github.com/repos/$REPO_SLUG/releases/assets/$1"
	fi
}

download_release_asset_id() {
	local id="$1" expected_size="$2" destination="$3" actual_size
	if [[ ! "$id" =~ ^[1-9][0-9]*$ || -z "$destination" ]] ||
		! decimal_in_closed_range "$expected_size" 1 2147483648; then
		echo "install.sh: refusing malformed release asset download" >&2
		return 2
	fi
	# Read at most one byte beyond the immutable API size. pipefail makes a
	# producer that overruns the bound fail, while the exact byte-count check
	# below catches both short and one-byte-overlong responses. This caps disk
	# exposure even if transport metadata and body disagree.
	if ! fetch_asset_octets "$id" |
		head -c "$((expected_size + 1))" >"$destination"; then
		echo "install.sh: exact asset $id download failed" >&2
		return 1
	fi
	actual_size="$(wc -c <"$destination" | tr -d '[:space:]')"
	if [[ "$actual_size" != "$expected_size" ]]; then
		echo "install.sh: exact asset $id size mismatch (API $expected_size, downloaded $actual_size)" >&2
		return 2
	fi
}

# Parse one emitted TOML string field. Required fields must occur exactly once;
# optional fields may occur zero or one time. This intentionally accepts only
# the release cutter's simple one-line string form rather than implementing a
# permissive shell TOML parser around security-sensitive identity fields.
toml_single_str() {
	local file="$1" key="$2" required="$3"
	awk -v wanted="$key" -v required="$required" '
		$0 ~ "^[[:space:]]*" wanted "[[:space:]]*=" {
			count += 1
			value = $0
			sub("^[[:space:]]*" wanted "[[:space:]]*=[[:space:]]*", "", value)
			if (value !~ /^"[^"]*"[[:space:]]*$/) {
				malformed = 1
			} else {
				sub(/^"/, "", value)
				sub(/"[[:space:]]*$/, "", value)
			}
		}
		END {
			if (malformed || count > 1 || (required == 1 && count != 1)) exit 2
			if (count == 1) print value
		}
	' "$file"
}

# Internal test seam: source this file to exercise the pure functions without
# parsing CLI arguments or touching the host. Never part of the public surface.
if [[ "${ATERM_INSTALL_LIBRARY_ONLY:-0}" == 1 ]]; then
	return 0 2>/dev/null || exit 0
fi

# --- uninstall: reverse exactly what this installer places ------------------
#
# Removes the five things install.sh creates — app bundle, the ONE `aterm`
# symlink, the source-built store, man pages, shell completions — plus the
# update token and its keychain item. Nothing else.
#
# EVERY removal is OWNERSHIP-CHECKED first. The installer writes into shared,
# user-owned locations (/Applications, ~/.local/bin, the XDG man and completion
# trees) that are full of things it did not put there, so "the path we would
# have written" is NOT sufficient reason to delete. A bundle must identify as
# com.aterm.aterm; the `aterm` on PATH must be a symlink resolving into a bundle
# or our store (a user's own hand-built binary at that path is left alone); a
# man page must be one this repo actually ships. Anything failing its check is
# reported as SKIPPED with the reason, never removed.
#
# --dry-run prints the same decisions and deletes nothing.
uninstall_everything() {
	local removed=0 skipped=0 act="remove"
	[[ "$DRY_RUN" -eq 1 ]] && act="would remove"

	_rm() { # _rm <path> <label>
		if [[ "$DRY_RUN" -eq 1 ]]; then
			echo "install.sh: $act $2: $1"
		elif rm -rf "$1" 2>/dev/null; then
			echo "install.sh: removed $2: $1"
		else
			echo "install.sh: SKIPPED $2 (cannot remove, try sudo): $1" >&2
			skipped=$((skipped + 1))
			return 1
		fi
		removed=$((removed + 1))
	}
	_skip() {
		echo "install.sh: SKIPPED $1 — $2" >&2
		skipped=$((skipped + 1))
	}

	# 1. app bundle — every candidate dir, but only genuine aterm bundles.
	# An explicit ATERM_INSTALL_DIR names THE install location — scan only it.
	# Scanning the defaults as well would let an override aimed at a scratch dir
	# reach out and delete the real /Applications/aterm.app.
	local dir app plist id
	local app_dirs=(/Applications "$HOME/Applications")
	[[ -n "${ATERM_INSTALL_DIR:-}" ]] && app_dirs=("$ATERM_INSTALL_DIR")
	for dir in "${app_dirs[@]}"; do
		app="$dir/aterm.app"
		[[ -d "$app" ]] || continue
		plist="$app/Contents/Info.plist"
		id=""
		[[ -f "$plist" ]] && id="$(defaults read "$plist" CFBundleIdentifier 2>/dev/null || true)"
		if [[ "$id" == "com.aterm.aterm" ]]; then
			_rm "$app" "app bundle"
		else
			_skip "$app" "not an aterm bundle (CFBundleIdentifier=${id:-unreadable})"
		fi
	done

	# 2. the `aterm` symlink — only when it still resolves into a bundle or our
	#    store. A real file there is someone's own build, not ours to delete.
	local bin="${ATERM_BIN_DIR:-$HOME/.local/bin}/aterm"
	local store="${ATERM_STORE_DIR:-$HOME/.local/lib/aterm/bin}"
	if [[ -L "$bin" ]]; then
		local target
		target="$(readlink "$bin" 2>/dev/null || true)"
		case "$target" in
		*/aterm.app/Contents/MacOS/* | "$store"/*) _rm "$bin" "aterm command" ;;
		*) _skip "$bin" "symlink points outside an aterm bundle or store ($target)" ;;
		esac
	elif [[ -e "$bin" ]]; then
		_skip "$bin" "not a symlink — a hand-built binary this installer did not place"
	fi

	# 3. source-built store (only the cargo-fallback lane creates it).
	[[ -d "$store" ]] && _rm "$store" "source-built toolset"

	# 4. man pages — only names this repo ships, so a foreign aterm*.1 is safe.
	local man_root="${ATERM_MAN_DIR:-$HOME/.local/share/man}" page base repo_man
	repo_man=""
	if self_on_disk; then
		repo_man="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd || true)/man"
	fi
	for page in "$man_root"/man[1-9]/aterm*.[1-9]; do
		[[ -e "$page" ]] || continue
		base="${page##*/}"
		if [[ -n "$repo_man" && -d "$repo_man" && ! -e "$repo_man/$base" ]]; then
			_skip "$page" "not a man page this repo ships"
			continue
		fi
		_rm "$page" "man page"
	done

	# 5. shell completions — the three exact paths install_cli_completions writes.
	local xdg_data="${XDG_DATA_HOME:-$HOME/.local/share}"
	local xdg_config="${XDG_CONFIG_HOME:-$HOME/.config}"
	local comp
	for comp in \
		"$xdg_data/bash-completion/completions/aterm-ctl" \
		"$xdg_data/zsh/site-functions/_aterm-ctl" \
		"$xdg_config/fish/completions/aterm-ctl.fish"; do
		[[ -e "$comp" ]] && _rm "$comp" "shell completion"
	done

	# 6. the update token, its keychain twin, and the support dir when EMPTY.
	#    The support dir also holds settings and staged updates, so it is only
	#    rmdir'd (never rm -rf'd) — a non-empty one is left exactly as it is.
	local support="$HOME/Library/Application Support/aterm"
	[[ -f "$support/update-token" ]] && _rm "$support/update-token" "update token"
	if [[ "$DRY_RUN" -eq 0 ]] && command -v security >/dev/null 2>&1; then
		if security find-generic-password -s aterm-update-token >/dev/null 2>&1; then
			security delete-generic-password -s aterm-update-token >/dev/null 2>&1 &&
				echo "install.sh: removed keychain item: aterm-update-token" &&
				removed=$((removed + 1))
		fi
	fi
	[[ -d "$support" && "$DRY_RUN" -eq 0 ]] && rmdir "$support" 2>/dev/null &&
		echo "install.sh: removed empty support dir: $support"

	if [[ "$removed" -eq 0 && "$skipped" -eq 0 ]]; then
		echo "install.sh: nothing installed by install.sh was found — nothing to do"
		return 0
	fi
	local verb="uninstall"
	[[ "$DRY_RUN" -eq 1 ]] && verb="dry run"
	local tail=""
	[[ "$skipped" -gt 0 ]] && tail=", $skipped skipped"
	echo "install.sh: $verb complete — $removed item(s)$tail"
	# User data is deliberately NOT touched: settings, themes, Trail Packs and
	# atpkg's installed toolchain outlive an uninstall on purpose.
	echo "install.sh: left in place: your settings/themes under $support, and any atpkg toolchain"
	[[ "$skipped" -gt 0 ]] && return 1
	return 0
}

TAG=""
TAG_EXPLICIT=0
DO_APP=1
DO_CLI=1
DO_TOKEN=1
DO_UNINSTALL=0
DRY_RUN=0
while [[ $# -gt 0 ]]; do
	case "$1" in
	-h | --help)
		usage
		exit 0
		;;
	-v | --version)
		[[ -n "${2:-}" ]] || { echo "install.sh: --version needs an argument" >&2; exit 2; }
		TAG="v${2#v}"
		TAG_EXPLICIT=1
		shift 2
		;;
	--no-cli)
		DO_CLI=0
		shift
		;;
	--no-app)
		DO_APP=0
		shift
		;;
	--no-token)
		DO_TOKEN=0
		shift
		;;
	--uninstall)
		DO_UNINSTALL=1
		shift
		;;
	--dry-run)
		DRY_RUN=1
		shift
		;;
	*)
		echo "install.sh: unknown argument: $1 (try --help)" >&2
		exit 2
		;;
	esac
done
if [[ "$DO_UNINSTALL" -eq 1 ]]; then
	uninstall_everything
	exit $?
fi
if [[ "$DRY_RUN" -eq 1 ]]; then
	echo "install.sh: --dry-run is only meaningful with --uninstall" >&2
	exit 2
fi
if [[ "$DO_APP" -eq 0 && "$DO_CLI" -eq 0 && "$DO_TOKEN" -eq 0 ]]; then
	echo "install.sh: --no-app --no-cli --no-token excludes everything — nothing to install" >&2
	exit 2
fi
if [[ "$TAG_EXPLICIT" -eq 1 ]] && ! canonical_numeric_tag "$TAG"; then
	echo "install.sh: --version must be a canonical release version — X.Y.Z (a retired two-component X.Y archive release is also accepted)" >&2
	exit 2
fi

# Repo slug: env override first; else (when run from a checkout) the single source
# of truth, [workspace.package] repository in Cargo.toml — same derivation as the
# binary's compiled-in default, so the private staging checkout targets itself and
# the public export (whose transform rewrites the owner) targets the mirror; else
# the canonical PUBLIC release repo, which is what a piped run installs from.
repo_slug_from_cargo() {
	local root
	# When piped (`… | bash`) there is no script path — skip straight to the default.
	self_on_disk || return 0
	root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)" || return 0
	[[ -f "$root/Cargo.toml" ]] || return 0
	awk -F'"' '/^\[workspace\.package\]/{f=1} f&&/^repository[[:space:]]*=/{print $2; exit}' "$root/Cargo.toml" |
		sed -E 's#^[a-z]+://github\.com/##; s#^git@github\.com:##; s#\.git$##; s#/$##'
}
REPO_SLUG="${ATERM_REPO_SLUG:-$(repo_slug_from_cargo)}"
: "${REPO_SLUG:=alabsystems/aterm}"

# --- pre-flight: decide what each half CAN do here (fail fast, skip loud) ------
APP_SKIP=""
APP_FATAL=""
APP_LANE=""
DEST=""
if [[ "$DO_APP" -eq 1 ]]; then
	if [[ "$(uname -s)" != "Darwin" ]]; then
		APP_SKIP="the released aterm.app is macOS-only"
	elif command -v gh >/dev/null 2>&1 && gh auth token >/dev/null 2>&1; then
		# An authenticated gh serves ANY slug, and is the only way into the
		# private staging repo. Decided ONCE here: every release/asset call
		# below rides the same lane, so metadata and octets never split
		# between credentials.
		APP_LANE=gh
	elif ! command -v curl >/dev/null 2>&1; then
		APP_SKIP="needs curl (anonymous public download) or an authenticated gh (brew install gh, then gh auth login)"
	elif curl -fsS -o /dev/null "https://api.github.com/repos/$REPO_SLUG"; then
		# The default lane: the PUBLIC release repo, fetched anonymously.
		APP_LANE=anon
	else
		APP_SKIP="cannot reach $REPO_SLUG anonymously — a private repo (the staging tree) needs an authenticated gh (brew install gh, then gh auth login); otherwise the network or the anonymous API rate limit is the problem"
	fi
	if [[ -n "$APP_LANE" ]]; then
		LIST_ERR=0
		if [[ -z "$TAG" ]]; then
			# The repo also publishes non-app releases (for example the atpkg
			# index), so enumerate every page and arbitrate the complete exact-name
			# appcast catalog. GitHub does not promise REST release row order.
			RELEASE_ROWS=""
			if [[ "$APP_LANE" == gh ]]; then
				if ! RELEASE_ROWS="$(gh api --paginate "repos/$REPO_SLUG/releases?per_page=100" \
					--jq '.[] | [.tag_name, (.draft | tostring), ([((.assets // [])[] | select(.name == "aterm-appcast.toml"))] | length | tostring)] | @tsv' \
					2>/dev/null)"; then
					LIST_ERR=1
				fi
			else
				# Anonymous pagination: same complete-catalog contract as gh's
				# --paginate. A short page ends the walk; the page cap keeps a
				# pathological catalog from spinning against the 60-requests/hour
				# anonymous limit (30 pages ⇒ 3000 releases, not a real state).
				RELEASE_PAGE=1
				while :; do
					if ! RELEASE_PAGE_JSON="$(curl -fsS -H "Accept: application/vnd.github+json" \
						"https://api.github.com/repos/$REPO_SLUG/releases?per_page=100&page=$RELEASE_PAGE")"; then
						LIST_ERR=1
						break
					fi
					RELEASE_PAGE_ROWS="$(anon_release_rows <<<"$RELEASE_PAGE_JSON")"
					[[ -n "$RELEASE_PAGE_ROWS" ]] || break
					RELEASE_ROWS="${RELEASE_ROWS:+$RELEASE_ROWS$'\n'}$RELEASE_PAGE_ROWS"
					[[ "$(printf '%s\n' "$RELEASE_PAGE_ROWS" | wc -l | tr -d '[:space:]')" -eq 100 ]] || break
					RELEASE_PAGE=$((RELEASE_PAGE + 1))
					[[ "$RELEASE_PAGE" -le 30 ]] || { LIST_ERR=1; break; }
				done
			fi
			if [[ "$LIST_ERR" -eq 0 ]]; then
				SELECT_STATUS=0
				TAG="$(select_authoritative_tag "$RELEASE_ROWS")" || SELECT_STATUS=$?
				if [[ "$SELECT_STATUS" -eq 2 ]]; then
					APP_FATAL="release catalog is malformed or ambiguous; refusing order-dependent fallback"
				fi
			fi
		fi
		if [[ -n "$APP_FATAL" ]]; then
			:
		elif [[ -z "$TAG" && "$LIST_ERR" -eq 1 ]]; then
			if [[ "$APP_LANE" == gh ]]; then
				APP_SKIP="could not list releases in $REPO_SLUG (bad/expired token, no repo access, or rate limit — try: gh auth status)"
			else
				APP_SKIP="could not list releases in $REPO_SLUG anonymously (network, or the GitHub API rate limit — retry later, or authenticate: gh auth login)"
			fi
		elif [[ -z "$TAG" ]]; then
			APP_SKIP="no current-scheme app release (a vMAJOR.MINOR.PATCH tag carrying aterm-appcast.toml) found in $REPO_SLUG — retired two-component releases are archive history and are never elected; name one with --version to install it anyway"
		else
			# Destination: explicit env wins; else /Applications, else ~/Applications.
			# (The in-app updater never relocates — it defers when its location isn't
			# writable, docs/RELEASING.md — so choosing a user-writable dir here is what
			# keeps self-update working for non-admin users.) Checked HERE so a bad
			# destination skips the half up front, not after the whole DMG download.
			DEST="${ATERM_INSTALL_DIR:-}"
			if [[ -z "$DEST" ]]; then
				if [[ -w /Applications ]]; then DEST=/Applications; else DEST="$HOME/Applications"; fi
			fi
			if ! mkdir -p "$DEST" 2>/dev/null || [[ ! -w "$DEST" ]]; then
				APP_SKIP="cannot create/write $DEST (set ATERM_INSTALL_DIR to a writable dir)"
			fi
		fi
	fi
fi

if [[ -n "$APP_FATAL" ]]; then
	echo "install.sh: REFUSED the app: $APP_FATAL" >&2
	exit 1
fi

CLI_SKIP=""
CLI_CARGO_SKIP=""
ROOT=""
BIN_DIR="${ATERM_BIN_DIR:-$HOME/.local/bin}"
# The private store for source-built toolset binaries: ONE name (`aterm`) is
# exposed on PATH as a symlink into here; the verb siblings ride alongside,
# resolved via current_exe — the same expose/bundle split as
# [workspace.metadata.atpkg] and the app bundle.
STORE_DIR="${ATERM_STORE_DIR:-$HOME/.local/lib/aterm/bin}"
if [[ "$DO_CLI" -eq 1 ]]; then
	# The destination gates BOTH sources (symlink + build) — check it first,
	# BEFORE any install work, so a bad destination skips the half up front.
	if ! mkdir -p "$BIN_DIR" 2>/dev/null || [[ ! -w "$BIN_DIR" ]]; then
		CLI_SKIP="cannot create/write $BIN_DIR (set ATERM_BIN_DIR to a writable dir)"
	else
		# Feasibility of the cargo FALLBACK only. The PREFERRED source — an
		# installed bundle shipping the tools — is probed at install time
		# (install_cli), because the app half hasn't run yet here and may be
		# about to install exactly the bundle the symlinks want.
		if self_on_disk; then
			ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." 2>/dev/null && pwd)" || ROOT=""
		fi
		if [[ -z "$ROOT" || ! -f "$ROOT/Cargo.toml" ]]; then
			CLI_CARGO_SKIP="building from source needs a checkout (a piped script has none) — git clone https://github.com/$REPO_SLUG && cd ${REPO_SLUG##*/} && tools/install.sh"
		elif ! command -v cargo >/dev/null 2>&1 || ! command -v rustc >/dev/null 2>&1; then
			CLI_CARGO_SKIP="building from source needs cargo (and rustc) — install via https://rustup.rs (or: brew install rust)"
		elif PINNED_CHANNEL="$(toml_single_str "$ROOT/rust-toolchain.toml" channel 0 2>/dev/null)" &&
			[[ -n "$PINNED_CHANNEL" ]] &&
			! rustup toolchain list 2>/dev/null | awk '{print $1}' | grep -Eq "^${PINNED_CHANNEL}(-|$)"; then
			# The checkout pins a CUSTOM toolchain (rust-toolchain.toml: the
			# linked Trust stage2 — rustup cannot download it) and
			# .cargo/config.toml injects -Ztrust-verify=off, which no stock
			# rustc parses — so a build without the pinned toolchain is doomed
			# on every machine but the operator's. FAILSAFE POLICY: detected
			# HERE as a loud skip naming the acquisition path, never as a
			# mid-flight abort of a build that cannot succeed.
			CLI_CARGO_SKIP="building from source needs the pinned '$PINNED_CHANNEL' rustup toolchain, which rustup cannot download — unpack the rustc/cargo/rust-std dist tarballs from https://github.com/alabsystems/trust/releases into one prefix, then: rustup toolchain link $PINNED_CHANNEL <prefix>"
		elif ! mkdir -p "$STORE_DIR" 2>/dev/null || [[ ! -w "$STORE_DIR" ]]; then
			# Pre-flighted HERE (failsafe policy: an unwritable destination
			# skips the half up front) — never discovered after the
			# multi-minute toolset build. Note `mkdir -p` succeeds on an
			# EXISTING unwritable dir, hence the explicit -w check.
			CLI_CARGO_SKIP="cannot create/write $STORE_DIR (set ATERM_STORE_DIR to a writable dir)"
		fi
	fi
fi

INSTALLED_ANY=0

# --- the app half: released aterm.app, verified, swapped into place ------------
install_app() {
	echo "install.sh: installing $REPO_SLUG $TAG"

	TMP="$(mktemp -d "${TMPDIR:-/tmp}/aterm-install.XXXXXX")"
	MNT="$TMP/mnt"
	STAGE=""
	OLD=""
	cleanup() {
		# Best-effort only, and never let a cleanup failure rewrite the exit status.
		set +e
		# Only a DMG is MOUNTED. The lean zip is expanded into this same path, and
		# calling hdiutil on a plain directory costs a pointless `sleep 2` plus a
		# second failed force-detach on every lean install — `rm -rf "$TMP"` below
		# is all that one needs.
		if [[ "${CONTAINER_KIND:-dmg}" == dmg && -d "$MNT" ]]; then
			hdiutil detach "$MNT" -quiet >/dev/null 2>&1 ||
				{ sleep 2; hdiutil detach "$MNT" -force -quiet >/dev/null 2>&1; }
		fi
		[[ -n "$STAGE" ]] && rm -rf "$STAGE"
		# If the swap was interrupted after the old bundle was set aside, restore it.
		if [[ -n "$OLD" && -d "$OLD" && ! -e "$DEST/aterm.app" ]]; then
			mv "$OLD" "$DEST/aterm.app"
		fi
		[[ -n "$OLD" ]] && rm -rf "$OLD"
		rm -rf "$TMP"
	}
	trap cleanup EXIT

	# Resolve and carry one exact manifest asset identity. Filename-pattern
	# downloads can silently pick an order-dependent duplicate and are forbidden.
	MANIFEST_RECORD="$(release_unique_asset_record "$TAG" 'aterm-appcast.toml' 1 5000000)" || exit 1
	IFS=$'\t' read -r MANIFEST_ID MANIFEST_SIZE <<<"$MANIFEST_RECORD"
	download_release_asset_id "$MANIFEST_ID" "$MANIFEST_SIZE" "$TMP/aterm-appcast.toml"
	if ! VERSION="$(toml_single_str "$TMP/aterm-appcast.toml" version 1)" ||
		! DMG_NAME="$(toml_single_str "$TMP/aterm-appcast.toml" dmg 1)" ||
		! SHA_WANT="$(toml_single_str "$TMP/aterm-appcast.toml" sha256 1)" ||
		! TEAM_MANIFEST="$(toml_single_str "$TMP/aterm-appcast.toml" team_id 0)" ||
		! MIN_OS="$(toml_single_str "$TMP/aterm-appcast.toml" min_os 0)"; then
		echo "install.sh: release $TAG has a malformed or duplicate manifest identity field" >&2
		exit 1
	fi
	validate_manifest_identity "$TAG" "$VERSION" "$DMG_NAME" "$SHA_WANT" || exit 1
	TEAM_WANT="${ATERM_TEAM_ID:-$TEAM_MANIFEST}"

	# --- pick the container: fat DMG, or the LEAN zip -------------------------
	# The DMG carries the batteries-included toolchain seal (~600 MB of signed
	# tarballs) so a first launch provisions the whole ALab toolset with no
	# network. The zip is the SAME signed, notarized bundle with that payload
	# stripped (~1/15 the bytes) — it already exists, is already published every
	# cut, and is what self-updates download.
	#
	# The ONE case that takes the lean container is ARCHITECTURE, and it is not a
	# preference — it is correctness. Every published ALab artifact is
	# aarch64-apple-darwin, so on an x86_64 Mac the seal can install exactly
	# nothing: the fat DMG would ship ~600 MB of provably unusable bytes that the
	# first launch then deletes. Not sending them is not "opting out of
	# batteries"; there are no batteries for that machine to receive.
	#
	# Deliberately NO user-facing knob here. Batteries are the product default,
	# and a flag to decline them would contradict that for a case nobody has
	# asked for — someone who truly wants the small container can install the
	# published zip directly, and someone who wants the toolchain gone after the
	# fact has `atpkg uninstall --all`.
	ZIP_NAME="$(toml_single_str "$TMP/aterm-appcast.toml" zip 0)" || ZIP_NAME=""
	ZIP_SHA="$(toml_single_str "$TMP/aterm-appcast.toml" zip_sha256 0)" || ZIP_SHA=""
	CONTAINER_KIND=dmg
	ASSET_NAME="$DMG_NAME"
	ASSET_SHA="$SHA_WANT"
	# HARDWARE, not the reporting process. `uname -m` answers for the running
	# process: `#!/usr/bin/env bash` takes whatever bash is first on PATH, so an
	# Intel-Homebrew /usr/local/bin/bash — or `arch -x86_64 bash`, or any
	# Rosetta-translated shell — reports x86_64 on an M-series Mac. Deciding
	# batteries from that would hand a real Apple Silicon machine the container
	# with no toolchain in it, which is the exact opposite of the intent, and the
	# user would never know why. `hw.optional.arm64` answers for the CPU.
	IS_APPLE_SILICON=0
	[[ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" == 1 ]] && IS_APPLE_SILICON=1
	if [[ -n "$ZIP_NAME" && -n "$ZIP_SHA" && "$IS_APPLE_SILICON" == 0 ]]; then
		# The SAME identity binds the DMG lane enforces, applied to the zip. Skipping
		# them here would have made the lean container the one download whose name
		# and digest shape nobody checked — a filename from the manifest joined
		# straight onto a temp path, and a digest compared without ever proving it
		# is a digest. The canonical-name bind is what stops a manifest naming some
		# other asset in the release; the 64-hex bind is what stops an empty or
		# malformed field from turning the later comparison into a no-op.
		if [[ "$ZIP_NAME" != "aterm-$VERSION-mac.zip" ]]; then
			echo "install.sh: manifest zip $ZIP_NAME is not canonical aterm-$VERSION-mac.zip" >&2
			exit 1
		fi
		if [[ ! "$ZIP_SHA" =~ ^[0-9a-fA-F]{64}$ ]]; then
			echo "install.sh: manifest zip_sha256 is not exactly 64 hexadecimal digits" >&2
			exit 1
		fi
		CONTAINER_KIND=zip
		ASSET_NAME="$ZIP_NAME"
		ASSET_SHA="$ZIP_SHA"
		echo "install.sh: Intel Mac — using the lean container ($ASSET_NAME)."
		echo "install.sh:   Identical signed app. The bundled toolchain is omitted because"
		echo "install.sh:   no ALab build is published for x86_64; atpkg installs it from"
		echo "install.sh:   the network as soon as one is."
	fi

	# Signing is OPTIONAL (Tier REPO): unsigned releases carry no
	# aterm-appcast.toml.sig at all and that absence is expected and tolerated.
	# When a signature IS present, duplicate exact-name signatures are never
	# resolved by order.
	if ! SIGNATURE_RECORDS="$(release_asset_records "$TAG" 'aterm-appcast.toml.sig')"; then
		echo "install.sh: could not inspect manifest signatures for release $TAG" >&2
		exit 1
	elif [[ -n "$SIGNATURE_RECORDS" ]]; then
		require_unique_asset_record "$SIGNATURE_RECORDS" 'aterm-appcast.toml.sig' 64 64 >/dev/null || exit 1
	fi
	echo "install.sh: BOOTSTRAP TRUST BOUNDARY: Ed25519 is not verified here; this lane trusts" >&2
	if [[ "${APP_LANE:-gh}" == gh ]]; then
		echo "  gh-authenticated repo metadata plus the manifest DMG hash (Tier REPO). Update signing is" >&2
	else
		echo "  TLS to api.github.com plus the manifest DMG hash (Tier REPO, anonymous). Update signing is" >&2
	fi
	echo "  optional; the installed updater only demands a signature if a key was compiled into it." >&2

	# Soft OS floor advisory. Validate both operands before comparison so
	# manifest text never enters Bash's arithmetic-expression evaluator.
	OS_VER="$(sw_vers -productVersion)"
	if [[ -n "$MIN_OS" ]]; then
		if ! canonical_numeric_tag "v$MIN_OS"; then
			echo "install.sh: release $TAG has a malformed min_os value '$MIN_OS'" >&2
			exit 1
		elif canonical_numeric_tag "v$OS_VER"; then
			compare_numeric_tags "v$OS_VER" "v$MIN_OS" || exit 1
			if [[ "$TAG_COMPARE_RESULT" -eq -1 ]]; then
				echo "install.sh: WARNING: macOS $OS_VER is below the release's minimum ($MIN_OS); the app may not launch" >&2
			fi
		else
			echo "install.sh: WARNING: cannot interpret local macOS version '$OS_VER'; skipping the minimum-OS advisory" >&2
		fi
	fi

	# Identity validation above makes this the canonical basename before it is
	# joined to TMP. Resolve exactly one matching API asset and download that ID.
	ASSET_RECORD="$(release_unique_asset_record "$TAG" "$ASSET_NAME" 1 2147483648)" || exit 1
	IFS=$'\t' read -r ASSET_ID ASSET_SIZE <<<"$ASSET_RECORD"
	# SAY WHAT IS ABOUT TO HAPPEN. This download went from ~51 MB to ~650 MB when the
	# toolchain moved into the DMG, and the script's output did not change by one
	# character: between "installing <slug> <tag>" and "sha256 verified" it prints
	# nothing, on a transport that is deliberately quiet (`curl -fsS`, `gh api` with
	# no meter). On a slow line that is many minutes of a `curl | bash` pipeline that
	# looks hung — the classic reason someone ^Cs an install half-written.
	if [[ "$CONTAINER_KIND" == dmg ]]; then
		echo "install.sh: downloading $ASSET_NAME ($((ASSET_SIZE / 1000000)) MB — includes the bundled ALab toolchain, so the first launch needs no network)"
	else
		echo "install.sh: downloading $ASSET_NAME ($((ASSET_SIZE / 1000000)) MB)"
	fi
	download_release_asset_id "$ASSET_ID" "$ASSET_SIZE" "$TMP/$ASSET_NAME"
	SHA_GOT="$(shasum -a 256 "$TMP/$ASSET_NAME" | awk '{print $1}')"
	if [[ "$SHA_GOT" != "$ASSET_SHA" ]]; then
		echo "install.sh: SHA-256 MISMATCH for $ASSET_NAME — refusing to install" >&2
		echo "  manifest: $ASSET_SHA" >&2
		echo "  download: $SHA_GOT" >&2
		exit 1
	fi
	echo "install.sh: sha256 verified (${SHA_GOT:0:12}…)"

	# Both containers land the bundle at "$MNT/aterm.app", so every check below —
	# codesign, the Team-ID designated requirement, Gatekeeper, the staged swap —
	# is identical for either. A stripped bundle verifies exactly like the fat one
	# (the payload sits in a `.lproj` directory sealed `optional = true`), which is
	# the property the whole lean lane rests on.
	if [[ "$CONTAINER_KIND" == zip ]]; then
		mkdir -p "$MNT"
		ditto -x -k "$TMP/$ASSET_NAME" "$MNT" || {
			echo "install.sh: could not expand $ASSET_NAME" >&2
			exit 1
		}
	else
		hdiutil attach "$TMP/$ASSET_NAME" -nobrowse -readonly -mountpoint "$MNT" -quiet
	fi
	[[ -d "$MNT/aterm.app" ]] || { echo "install.sh: no aterm.app inside $ASSET_NAME" >&2; exit 1; }

	codesign --verify --deep --strict "$MNT/aterm.app" || {
		echo "install.sh: code-signature verification FAILED — refusing to install" >&2
		exit 1
	}
	if [[ -n "$TEAM_WANT" ]]; then
		# Pin the whole Developer-ID chain for the team, not just a printed string —
		# the same designated requirement the in-app updater enforces (aterm-update).
		[[ "$TEAM_WANT" =~ ^[A-Za-z0-9]+$ ]] || {
			echo "install.sh: pinned team_id '$TEAM_WANT' is not alphanumeric — refusing to install" >&2
			exit 1
		}
		REQ="=anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] exists and certificate leaf[field.1.2.840.113635.100.6.1.13] exists and certificate leaf[subject.OU] = \"$TEAM_WANT\""
		codesign --verify --deep --strict -R "$REQ" "$MNT/aterm.app" || {
			echo "install.sh: bundle is not Developer-ID signed by team $TEAM_WANT — refusing to install" >&2
			exit 1
		}
		spctl -a -t exec "$MNT/aterm.app" || {
			echo "install.sh: Gatekeeper assessment (notarization) FAILED — refusing to install" >&2
			exit 1
		}
		echo "install.sh: signature verified (Developer ID, Team ID $TEAM_WANT, notarized)"
	else
		echo "install.sh: signature verified (development build — ad-hoc signed, no pinned Team ID)"
	fi

	# Stage next to the destination, swap by rename, delete the old copy last — either
	# the old or the new bundle exists at $DEST/aterm.app at every instant, and an
	# interrupt mid-swap restores the old one (see cleanup).
	# The staging name must be VISIBLE (no leading dot): Spotlight never indexes
	# content under dot-paths, and the final rename does not backfill the index —
	# a dot-staged install lands in /Applications permanently invisible to
	# Spotlight (mdimport after the fact does not recover it).
	STAGE="$DEST/aterm.app.installing.$$"
	ditto "$MNT/aterm.app" "$STAGE"
	if [[ -e "$DEST/aterm.app" ]]; then
		OLD="$DEST/.aterm.app.old.$$"
		mv "$DEST/aterm.app" "$OLD"
	fi
	mv "$STAGE" "$DEST/aterm.app"
	STAGE=""
	if [[ -n "$OLD" ]]; then
		rm -rf "$OLD" 2>/dev/null ||
			echo "install.sh: NOTE: could not fully remove the previous copy (left at $OLD; remove with: sudo rm -rf '$OLD')" >&2
		OLD=""
	fi

	# Nudge Spotlight (the staged folder was indexed as a plain directory; the
	# rename just made it an .app bundle) and LaunchServices, so the app is
	# findable the moment the installer exits.
	mdimport "$DEST/aterm.app" >/dev/null 2>&1 || true
	/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister \
		-f "$DEST/aterm.app" >/dev/null 2>&1 || true

	if pgrep -f '/aterm\.app/Contents/MacOS/aterm' >/dev/null 2>&1; then
		echo "install.sh: NOTE: aterm is currently running — the new version takes effect on next launch"
	fi

	echo "install.sh: installed aterm $VERSION -> $DEST/aterm.app"
	echo "  launch:  open '$DEST/aterm.app'"
	if [[ "${APP_LANE:-gh}" == gh ]]; then
		echo "  updates: automatic (silent, verified); it reuses your gh credential — see docs/RELEASING.md"
	else
		echo "  updates: automatic (silent, verified) once the token half below provisions a credential — see docs/RELEASING.md"
	fi
	echo "           opt out with ATERM_NO_AUTO_UPDATE=1"
	INSTALLED_ANY=1
}

# --- the cli half: the one `aterm` command, preferring the installed bundle ----
# Newer releases ship the CLI terminal inside the bundle as `aterm-cli` (its
# cargo binary is `aterm`; that name belongs to the GUI in Contents/MacOS)
# next to the verb siblings — so the preferred install is ONE symlink into the
# bundle: no cargo, piped-script friendly, and the link tracks in-place app
# updates (the updater swaps aterm.app at the same path).

# The first installed bundle whose toolset can back the `aterm` symlink,
# echoed as the exact TARGET to link. Two generations qualify:
#   one-binary (current): Contents/MacOS/aterm IS everything — it identifies
#     itself as "aterm <version>" (the old GUI-only executable in that slot said
#     "aterm-gui <version>", which must NOT be linked: it had no session mode).
#     The name is the discriminator; the version spelling is not parsed here.
#   toolset (previous):   aterm-cli + the four verb siblings all present.
# Candidates: the app half's fresh install first, then the env override, then
# the standard app dirs. The --version probes filter truncated/foreign copies.
find_bundle_cli() {
	[[ "$(uname -s)" == "Darwin" ]] || return 0
	local c m b ok
	for c in ${DEST:+"$DEST"} ${ATERM_INSTALL_DIR:+"$ATERM_INSTALL_DIR"} /Applications "$HOME/Applications"; do
		m="$c/aterm.app/Contents/MacOS"
		if [[ -x "$m/aterm" ]] && "$m/aterm" --version 2>/dev/null | grep -q '^aterm '; then
			echo "$m/aterm"
			return 0
		fi
		ok=1
		for b in aterm-cli aterm-ctl atpkg aterm-fleet aterm-drive; do
			[[ -x "$m/$b" ]] || { ok=0; break; }
		done
		if [[ "$ok" -eq 1 ]] && "$m/aterm-cli" --version >/dev/null 2>&1; then
			echo "$m/aterm-cli"
			return 0
		fi
	done
	return 0
}

install_cli() {
	local target
	target="$(find_bundle_cli)"
	if [[ -n "$target" ]]; then
		# ONE name on PATH (the [workspace.metadata.atpkg] expose declaration):
		# `aterm` alone — a symlink at the bundle's one binary (or, against a
		# previous-generation bundle, its aterm-cli front door).
		ln -sfn "$target" "$BIN_DIR/aterm"
		echo "install.sh: linked $("$BIN_DIR/aterm" --version) -> $BIN_DIR/aterm"
		echo "  a symlink into ${target%/Contents/MacOS/*} — it follows the app's silent auto-updates."
		if [[ "$target" == */MacOS/aterm ]]; then
			echo "  ONE command: \`aterm\` is the terminal, the window (--window), and every verb (aterm help / ctl / pkg / fleet / drive)"
		else
			echo "  ONE command: \`aterm\` fronts every verb (aterm help / ctl / pkg / fleet / drive); this release predates --window"
		fi
	elif [[ -n "$CLI_CARGO_SKIP" ]]; then
		# Neither source is available here: no installed bundle ships the
		# toolset (older release, or no app), and the build fallback is
		# impossible for the pre-flighted reason.
		echo "install.sh: SKIPPED the CLI (aterm): no installed aterm.app ships the toolset (newer releases do — update or reinstall the app first), and $CLI_CARGO_SKIP" >&2
		return 0
	else
		install_cli_from_source
	fi
	retire_exposed_siblings
	install_cli_manpages
	install_cli_completions
	cli_path_hint
	INSTALLED_ANY=1
}

install_cli_from_source() {
	echo "install.sh: building the aterm toolset at $(git -C "$ROOT" rev-parse --short=12 HEAD 2>/dev/null || echo '(not a git checkout)')" \
		"— run \`git pull --ff-only\` first for the latest main"
	# Pin the target dir (a global CARGO_TARGET_DIR / build.target-dir redirect
	# would strand the fresh binaries elsewhere) but NEVER a --target triple:
	# cargo withholds [target.*] rustflags from HOST units (build scripts, proc
	# macros, and their dependencies) whenever an explicit target is set — the
	# flag, CARGO_BUILD_TARGET, or [build] target — and this repo compiles with
	# Trust, whose one verification opt-out lives in exactly those rustflags
	# (.cargo/config.toml). Under a pinned triple the host units verify
	# strictly and the build fails closed (first casualty: shlex, under cc). A
	# plain host build applies the opt-out to every unit and lands the binary
	# at the ONE deterministic path target/release/. CARGO_BUILD_TARGET is
	# unset for the same reason; a [build] target redirect in user cargo
	# config cannot be neutralized from here, so the freshness proof below
	# turns that into a loud abort instead of a stale (or missing) install.
	local rel="$ROOT/target/release"
	# Existence-after-build IS the freshness proof: cargo always re-links a
	# deleted output, so a redirected build can never leave a previous build's
	# binary here to be silently installed.
	rm -f "$rel/aterm"
	# ONE binary carries everything: the session, the window, and every verb
	# in-process (`-p aterm` pulls the whole library graph).
	(cd "$ROOT" && env -u CARGO_BUILD_TARGET cargo build --release -p aterm --target-dir "$ROOT/target")
	if [[ ! -x "$rel/aterm" ]]; then
		echo "install.sh: the build finished but produced no $rel/aterm — a [build] target in your cargo config redirected it; remove that setting (or install the released app instead: tools/install.sh --no-cli)" >&2
		exit 1
	fi
	# STORE_DIR was pre-flighted (created + writability-checked) before the build.
	install -m 755 "$rel/aterm" "$STORE_DIR/"
	# argv0 compat aliases beside it (matching the bundle's symlinks), so
	# in-session \`aterm-ctl …\` scripts and \$ATERM_CTL keep resolving.
	local alias
	for alias in aterm-cli aterm-ctl atpkg aterm-fleet aterm-drive aterm-gui; do
		ln -sfn aterm "$STORE_DIR/$alias"
	done
	# rm first: a leftover REGULAR FILE from a pre-store install must not
	# survive as a stale copy shadowing the store.
	rm -f "$BIN_DIR/aterm"
	ln -sfn "$STORE_DIR/aterm" "$BIN_DIR/aterm"
	echo "install.sh: installed the ONE binary -> $STORE_DIR ($("$BIN_DIR/aterm" --version))"
	echo "  ONE command on PATH: $BIN_DIR/aterm — the terminal, the window (--window), and every verb"
}

# Earlier installs (and the pre-one-command layout) exposed `aterm-ctl` on
# PATH next to `aterm`. The front door supersedes it (`aterm ctl …`), so
# retire OUR previous copy — and only ours: a symlink into an aterm.app
# bundle or the store, or a regular file that identifies itself as aterm-ctl.
# Anything else under that name is left alone.
retire_exposed_siblings() {
	local p="$BIN_DIR/aterm-ctl" tgt=""
	[[ -e "$p" || -L "$p" ]] || return 0
	if [[ -L "$p" ]]; then
		tgt="$(readlink "$p")"
		case "$tgt" in
		*/aterm.app/Contents/MacOS/*ctl | "$STORE_DIR"/*) ;;
		*) return 0 ;;
		esac
	elif ! "$p" --version 2>/dev/null | grep -q '^aterm-ctl '; then
		return 0
	fi
	rm -f "$p"
	echo "install.sh: retired $p — \`aterm ctl\` is the front door now (the sibling still ships, co-located)"
}

install_cli_manpages() {
	# Man pages (best-effort, NON-FATAL): copy man/*.N into the XDG man tree so
	# `man aterm-ctl` / `man aterm` / `man aterm-gui` work. Never fails the install —
	# the binaries are already in place; a missing man/ dir or an unwritable dest is
	# silently skipped, as is the checkout-less symlink path (ROOT empty — man
	# pages live in the repo, not the bundle). Section = the trailing digit of
	# each filename (all .1 today).
	MAN_ROOT="${ATERM_MAN_DIR:-$HOME/.local/share/man}"
	if [[ -n "$ROOT" && -d "$ROOT/man" ]]; then
		man_n=0
		for page in "$ROOT"/man/*.[1-9]; do
			[[ -e "$page" ]] || continue # unmatched glob: nothing to install
			dest="$MAN_ROOT/man${page##*.}"
			if mkdir -p "$dest" 2>/dev/null && install -m 644 "$page" "$dest/" 2>/dev/null; then
				man_n=$((man_n + 1))
			fi
		done
		if [[ "$man_n" -gt 0 ]]; then
			echo "install.sh: installed $man_n man page(s) -> $MAN_ROOT (e.g. man aterm-ctl)"
			# ~/.local/share/man is a standard XDG man location most setups already
			# search; only nudge when MANPATH is explicitly set and omits it.
			case ":${MANPATH:-}:" in
			:: | *":$MAN_ROOT:"*) ;;
			*) echo "install.sh: NOTE: $MAN_ROOT is not in \$MANPATH — add it to run \`man aterm-ctl\`" >&2 ;;
			esac
		fi
	fi
}

install_cli_completions() {
	# Shell completions (best-effort, NON-FATAL): each is GENERATED from the
	# just-installed front door (`aterm ctl --completions <shell>`) and dropped in
	# the conventional per-user dir. A dir that can't be created/written skips just
	# that shell — the binary is already in place, so this never fails the install.
	local ac="$BIN_DIR/aterm"
	[[ -x "$ac" ]] || return 0
	local xdg_data="${XDG_DATA_HOME:-$HOME/.local/share}"
	local xdg_config="${XDG_CONFIG_HOME:-$HOME/.config}"
	# shell:destination pairs — the per-user completion locations for each shell
	# (zsh's site-functions dir is a conventional $fpath entry; the notice below
	# reminds the user to have it on fpath).
	local entry shell dest dir installed=""
	for entry in \
		"bash:$xdg_data/bash-completion/completions/aterm-ctl" \
		"zsh:$xdg_data/zsh/site-functions/_aterm-ctl" \
		"fish:$xdg_config/fish/completions/aterm-ctl.fish"; do
		shell="${entry%%:*}"
		dest="${entry#*:}"
		dir="$(dirname "$dest")"
		# mkdir -p succeeds on an existing unwritable dir, hence the explicit -w.
		mkdir -p "$dir" 2>/dev/null || continue
		[[ -w "$dir" ]] || continue
		# Write to a temp then rename, so a mid-write failure never leaves a
		# half-written completion file behind.
		if "$ac" ctl --completions "$shell" >"$dest.tmp.$$" 2>/dev/null && [[ -s "$dest.tmp.$$" ]]; then
			mv "$dest.tmp.$$" "$dest" 2>/dev/null && installed="${installed:+$installed, }$shell"
		else
			rm -f "$dest.tmp.$$" 2>/dev/null
		fi
	done
	[[ -n "$installed" ]] && echo "install.sh: installed shell completions ($installed) — restart your shell to load them"
	return 0
}

# --- update token: the credential for a REPOINTED update source ---------------
#
# History: the update channel used to be a private repo, so the updater needed a
# token to read it at all, and an install on a `gh`-authenticated developer Mac
# "just worked" while every other machine silently never updated. That is no
# longer the shape. The compiled-in channel is the PUBLIC repo and the updater
# reads it with no credential; a token only raises the anonymous API cadence.
#
# So this half is NOT what decides whether a Mac updates. Per
# crates/aterm-update-core/src/token.rs (`needs_ambient_credential` + `walk`):
# for the compiled-in channel the chain consults ONLY an explicit
# $ATERM_UPDATE_TOKEN and never touches the keychain or the file written below.
# The ambient chain — keychain, this 0600 file, $GITHUB_TOKEN, $GH_TOKEN, `gh
# auth token` — runs only when $ATERM_UPDATE_OWNER/_REPO repoint the source,
# which is the one way to reach a repo that can require authentication.
#
# Provisioning it anyway is cheap and keeps that repointed case working without
# `gh` on PATH (a Finder-launched .app has a minimal one) — but a machine
# without it is fine, and nothing here may claim otherwise.
#
# It is idempotent (a matching token is left alone), it NEVER prints the token,
# and it is skippable with --no-token. No failure here is fatal.
UPDATE_TOKEN_DIR="$HOME/Library/Application Support/aterm"
UPDATE_TOKEN_FILE="$UPDATE_TOKEN_DIR/update-token"

# Whether $1 is a well-formed token by the SAME rule the app enforces
# (`valid_token` in token.rs: [A-Za-z0-9_-], 1..=512). Keeping the two in step
# matters — provisioning a value the app will refuse is worse than not
# provisioning at all, because it looks done.
well_formed_token() {
	local t="$1"
	[[ -n "$t" && ${#t} -le 512 && "$t" =~ ^[A-Za-z0-9_-]+$ ]]
}

provision_update_token() {
	# macOS-only: the updater itself is macOS-only (aterm_update::enabled), so a
	# token elsewhere would be a file nothing reads.
	[[ "$(uname -s)" == "Darwin" ]] || return 0

	local tok=""
	if [[ -n "${ATERM_UPDATE_TOKEN:-}" ]]; then
		tok="$ATERM_UPDATE_TOKEN"
	elif command -v gh >/dev/null 2>&1; then
		tok="$(gh auth token 2>/dev/null || true)"
	fi
	# Trim only LEADING/TRAILING whitespace — exactly what the app's validation
	# chokepoint does. Deleting whitespace everywhere would be worse than useless:
	# it would turn a garbage value with an interior space into something that
	# passes `well_formed_token` and gets provisioned as a real credential.
	tok="${tok#"${tok%%[![:space:]]*}"}"
	tok="${tok%"${tok##*[![:space:]]}"}"

	if [[ -z "$tok" ]]; then
		# Only shout if there is genuinely no other way in. A machine with the
		# keychain item or an existing 0600 file is already provisioned and does
		# not need a lecture.
		if [[ -r "$UPDATE_TOKEN_FILE" ]] ||
			security find-generic-password -s aterm-update-token -w >/dev/null 2>&1; then
			INSTALLED_ANY=1
			echo "install.sh: update token already provisioned (existing file or keychain item) — left alone"
			return 0
		fi
		# NOT a warning: the compiled-in channel is public and updates
		# anonymously. Only a machine that repoints the updater at a private
		# repo needs this file, and it can add it later.
		# NOT a warning: the compiled-in channel is public, so this Mac updates
		# without any credential — just on the slower anonymous interval.
		echo "install.sh: no GitHub token available — the update channel is public, so this Mac"
		echo "install.sh:   still auto-updates. Unauthenticated checks share ~60 GitHub requests per"
		echo "install.sh:   hour per IP, so it checks about every 15 minutes instead of every 75s."
		echo "install.sh:   To get the faster cadence, export ATERM_UPDATE_TOKEN for the app — on the"
		echo "install.sh:   public channel that is the ONLY token source the updater consults."
		# NOT "$0": piped as `… | bash` that is literally "bash". Name the
		# documented invocation instead, which is correct however this ran.
		echo "install.sh:   The file this step writes is read only when ATERM_UPDATE_OWNER/_REPO point"
		echo "install.sh:   the updater at another repo; add it later with:"
		echo "install.sh:     gh auth login && tools/install.sh --no-app --no-cli"
		echo "install.sh:   Check update health any time with:  aterm ctl update status"
		return 0
	fi

	if ! well_formed_token "$tok"; then
		echo "install.sh: WARNING: the available GitHub token is malformed, so it was NOT provisioned" >&2
		echo "install.sh:   (expected [A-Za-z0-9_-]; got ${#tok} characters). Updates from the public" >&2
		echo "install.sh:   channel are unaffected; re-authenticate with \`gh auth login\` if you need a" >&2
		echo "install.sh:   token for a repointed, private update source." >&2
		return 0
	fi

	# Already provisioned with this exact token → nothing to change. Still counts
	# as a satisfied half, so `--no-app --no-cli` (the repair invocation) reports
	# success rather than "nothing was installed".
	if [[ -r "$UPDATE_TOKEN_FILE" ]] && [[ "$(cat "$UPDATE_TOKEN_FILE" 2>/dev/null)" == "$tok" ]]; then
		INSTALLED_ANY=1
		echo "install.sh: update token already provisioned -> $UPDATE_TOKEN_FILE (unchanged)"
		return 0
	fi

	if ! mkdir -p "$UPDATE_TOKEN_DIR" 2>/dev/null; then
		echo "install.sh: WARNING: could not create $UPDATE_TOKEN_DIR — the update token was NOT provisioned (public-channel updates are unaffected)" >&2
		return 0
	fi
	chmod 700 "$UPDATE_TOKEN_DIR" 2>/dev/null || true
	# Write to a per-pid temp created under umask 077 (so the bytes are never on
	# disk world-readable, not even briefly), then rename over the destination:
	# the live file is atomically the old token or the new one, never a partial.
	local tmp="$UPDATE_TOKEN_FILE.$$.tmp"
	if ! (
		umask 077
		printf '%s' "$tok" >"$tmp"
	) 2>/dev/null; then
		rm -f "$tmp" 2>/dev/null
		echo "install.sh: WARNING: could not write $UPDATE_TOKEN_FILE — the update token was NOT provisioned (public-channel updates are unaffected)" >&2
		return 0
	fi
	chmod 600 "$tmp" 2>/dev/null || true
	if ! mv -f "$tmp" "$UPDATE_TOKEN_FILE" 2>/dev/null; then
		rm -f "$tmp" 2>/dev/null
		echo "install.sh: WARNING: could not install $UPDATE_TOKEN_FILE — the update token was NOT provisioned (public-channel updates are unaffected)" >&2
		return 0
	fi
	INSTALLED_ANY=1
	echo "install.sh: provisioned the update token (0600) -> $UPDATE_TOKEN_FILE (used only if you repoint the updater at a private repo)"
}

cli_path_hint() {
	case ":$PATH:" in
	*":$BIN_DIR:"*) ;;
	*)
		case "${SHELL##*/}" in
		zsh) RC="$HOME/.zshrc" ;;
		bash) RC="$HOME/.bashrc" ;;
		*) RC="your shell profile" ;;
		esac
		echo "install.sh: NOTE: $BIN_DIR is not on your PATH — add it in $RC, e.g.:" >&2
		echo "  export PATH=\"$BIN_DIR:\$PATH\"" >&2
		;;
	esac
}

# --- run what's possible, skip the rest loudly, fail only if nothing ran -------
if [[ "$DO_APP" -eq 1 ]]; then
	if [[ -n "$APP_SKIP" ]]; then
		echo "install.sh: SKIPPED the app: $APP_SKIP" >&2
	else
		install_app
	fi
fi
if [[ "$DO_CLI" -eq 1 ]]; then
	if [[ -n "$CLI_SKIP" ]]; then
		echo "install.sh: SKIPPED the CLI (aterm): $CLI_SKIP" >&2
	else
		install_cli
	fi
fi
if [[ "$DO_TOKEN" -eq 1 ]]; then
	provision_update_token
fi
if [[ "$INSTALLED_ANY" -eq 0 ]]; then
	echo "install.sh: nothing was installed" >&2
	exit 1
fi
