#!/usr/bin/env bash
# Guard `vendor/llama-cpp-2` against silent drift from the pinned upstream
# release. The vendored tree is the crates.io `llama-cpp-2` release plus the
# safe abort-lifetime extension; nothing else may differ. Both the root
# workspace and tools/spike point `[patch.crates-io]` at it, so an unnoticed
# edit here ships into every binary with no compiler or lockfile signal.
#
# Ground truth is the committed digest manifest next to this script
# (vendor-llama-cpp-2.sha256): one sha256 per file of the published .crate.
# A normal run needs nothing else — no tarball, no registry cache, no network
# — because both lockfiles resolve llama-cpp-2 through `[patch.crates-io]` to
# vendor/, so cargo never downloads that tarball and a CI runner never has
# one. Every path whose vendored bytes differ from the manifest must appear in
# the allowlist with the digest of the vendored file and the reason it is
# patched, so neither a new difference nor a changed patch can pass as
# "intentional", and a patch that silently disappears fails too.
#
# When a tarball does happen to be at hand (a dev box, or
# COMPME_VENDOR_CRATE_PATH), the run additionally re-derives the manifest from
# it, so a hand-doctored manifest is caught; the output says which of the two
# assurance levels you got. `--write-manifest` regenerates the manifest from a
# tarball and is the only mode that requires one.
#
# Bumping llama-cpp-2 means re-stamping expected_version and upstream_sha256,
# re-running --write-manifest, and re-stamping every allowlist digest, all in
# the same commit as the vendored rebase.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

# Pinned upstream release. Cross-checked against the three `=x.y.z` Cargo.toml
# pins and the vendored manifest, so a bump cannot land in only some of them.
expected_version="0.1.146"
# sha256 of the published llama-cpp-2 0.1.146 registry tarball, as cargo
# verified it on download. Recorded in the digest manifest's header and
# re-checked against any local tarball: this script performs NO network
# access, so it adds no egress host to a product whose privacy gate reviews
# every one (check-privacy-policy.sh).
upstream_sha256="f3b0f368c76cc0fe475e8257aeeec269e0d6569bd48b1f503efd0963fc3ee397"

manifest_path="$repo_root/tools/release/vendor-llama-cpp-2.sha256"

usage() {
  echo "usage: check-vendor-drift.sh [--self-test | --write-manifest]" >&2
}

# kind|path|sha256-of-the-vendored-file|why it differs from upstream
#   patched = present upstream and deliberately modified here
#   added   = absent upstream and deliberately added here
allowlist() {
  cat <<'LIST'
patched|src/context.rs|cfe41780d19f908e26116cd95c7f3fe937ed9feff59c9c57cdf1a57792f53300|abort-lifetime extension: AbortCallbackState plus the LlamaContext field that keeps the callback data alive until llama_free returns
patched|src/context/params.rs|93d0754f3dff072f9ee724b56007dcf9a83483a0cd17dec93eedb47fe85f6e82|abort-lifetime extension: with_abort_flag / with_abort_poll_observer keep the raw callback pointer out of the Send+Sync params struct
patched|src/model.rs|594ebd3d700b2a1ced373b8ebfdf1282904cfe54bc906d10bf0c6c0bfd3ee0a6|abort-lifetime extension: installs llama_set_abort_callback and hands the owning Arc to LlamaContext
patched|README.md|d5ed950cb00fcaf4ffdbf2827a97524afafbb7324fee98f4061916f4c813127b|whitespace only: trailing spaces stripped when the crate was vendored in 52b509b; identical once trailing whitespace is normalised
patched|src/grammar/json.gbnf|ee7ecf3eb64f4d7f9a61dad46b4a24ca20c15671d41783e949b90f9bc98d1fce|whitespace only: trailing spaces stripped when the crate was vendored in 52b509b; identical once trailing whitespace is normalised
added|LICENSE-APACHE|a6cba85bc92e0cff7a450b1d873c0eaa2e9fc96bf472df0247a26bec77bf3ff9|redistribution compliance: the .crate ships no license text; added in 6a58e89
added|LICENSE-MIT|1c9043c09747e73ea87a2ab2aaa51d62c1c091932f7c88d0b1f3245eb7fcb1b6|redistribution compliance: the .crate ships no license text; added in 6a58e89
LIST
}

sha256_of() {
  file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | cut -d' ' -f1
  else
    echo "vendor drift check failed: neither sha256sum nor shasum is available" >&2
    return 1
  fi
}

# `.cargo-ok` is cargo's extraction marker, not crate content: the extracted
# registry copy has it and the .crate tarball does not. Ignoring it on both
# sides keeps the comparison identical whichever source is used.
list_tree() {
  dir="$1"
  (cd "$dir" && find . -type f ! -name .cargo-ok | sed 's|^\./||' | LC_ALL=C sort)
}

# Manifest body format: "<sha256>  <path>", LC_ALL=C sorted. The two-space
# separator matches sha256sum's own output, and fixes the field offsets: the
# digest is bytes 1-64 and the path starts at byte 67, so a path may contain
# anything but a newline.
digest_tree() {
  dir="$1"
  list_tree "$dir" | while IFS= read -r tree_path; do
    printf '%s  %s\n' "$(sha256_of "$dir/$tree_path")" "$tree_path"
  done
}

manifest_paths() {
  awk '{ print substr($0, 67) }' "$1"
}

manifest_digest_of() {
  awk -v want="$2" 'substr($0, 67) == want { print substr($0, 1, 64); exit }' "$1"
}

# Read the committed manifest into $2 as bare "<sha256>  <path>" lines.
# A missing or malformed manifest is a hard failure: it is ground truth, not
# a cache that may be absent.
read_manifest() {
  src="$1"
  out="$2"
  if [ ! -f "$src" ]; then
    echo "vendor drift check failed: digest manifest $src is missing; it is committed ground truth, regenerate it with --write-manifest" >&2
    return 1
  fi
  header_version="$(sed -n 's/^# version: \(.*\)$/\1/p' "$src" | head -1)"
  if [ "$header_version" != "$expected_version" ]; then
    echo "vendor drift check failed: malformed digest manifest $src: header records version ${header_version:-<none>}, this checker expects $expected_version" >&2
    return 1
  fi
  header_sha="$(sed -n 's/^# tarball-sha256: \(.*\)$/\1/p' "$src" | head -1)"
  if [ "$header_sha" != "$upstream_sha256" ]; then
    echo "vendor drift check failed: malformed digest manifest $src: header records tarball-sha256 ${header_sha:-<none>}, this checker expects $upstream_sha256" >&2
    return 1
  fi
  grep -v -e '^#' -e '^[[:space:]]*$' "$src" >"$out" || true
  if [ ! -s "$out" ]; then
    echo "vendor drift check failed: malformed digest manifest $src: it records no file digests" >&2
    return 1
  fi
  bad_line="$(grep -n -v -E '^[0-9a-f]{64}  [^[:space:]].*$' "$out" | head -1 || true)"
  if [ -n "$bad_line" ]; then
    echo "vendor drift check failed: malformed digest manifest $src: entry $(printf '%s' "$bad_line" | cut -d: -f1) is not \"<sha256>  <path>\"" >&2
    return 1
  fi
  dupe="$(manifest_paths "$out" | LC_ALL=C sort | uniq -d | head -1)"
  if [ -n "$dupe" ]; then
    echo "vendor drift check failed: malformed digest manifest $src: duplicate path $dupe" >&2
    return 1
  fi
}

# Compare a vendored tree against the upstream digests in an entries file,
# allowing only the paths in the allowlist file (same line format as
# `allowlist`).
compare_manifest() {
  entries="$1"
  vendor="$2"
  allow_file="$3"
  work="$(mktemp -d "${TMPDIR:-/tmp}/compme-vendor-drift-cmp.XXXXXX")"

  manifest_paths "$entries" >"$work/upstream.list"
  list_tree "$vendor" >"$work/vendor.list"
  LC_ALL=C sort -u "$work/upstream.list" "$work/vendor.list" >"$work/union.list"
  : >"$work/seen.list"

  failed=0
  allowed=0
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    upstream_sha="$(manifest_digest_of "$entries" "$path")"
    if [ -f "$vendor/$path" ]; then
      vendor_sha="$(sha256_of "$vendor/$path")"
    else
      vendor_sha=""
    fi
    if [ -n "$upstream_sha" ] && [ -n "$vendor_sha" ]; then
      if [ "$upstream_sha" = "$vendor_sha" ]; then
        continue
      fi
      kind="patched"
    elif [ -n "$vendor_sha" ]; then
      kind="added"
    else
      echo "vendor drift check failed: $path is missing from the vendored tree (upstream ships it)" >&2
      failed=1
      continue
    fi

    entry="$(awk -F'|' -v p="$path" '$2 == p { print; exit }' "$allow_file")"
    if [ -z "$entry" ]; then
      echo "vendor drift check failed: unexpected $kind file $path is not on the allowlist" >&2
      failed=1
      continue
    fi
    printf '%s\n' "$path" >>"$work/seen.list"
    allow_kind="$(printf '%s' "$entry" | cut -d'|' -f1)"
    allow_sha="$(printf '%s' "$entry" | cut -d'|' -f3)"
    if [ "$allow_kind" != "$kind" ]; then
      echo "vendor drift check failed: allowlist records $path as $allow_kind, the tree shows $kind" >&2
      failed=1
      continue
    fi
    if [ "$vendor_sha" != "$allow_sha" ]; then
      echo "vendor drift check failed: allowlisted $path changed: expected sha256 $allow_sha, found $vendor_sha" >&2
      failed=1
      continue
    fi
    allowed=$((allowed + 1))
  done <"$work/union.list"

  # A patch that vanished is drift too: every allowlist entry must still apply.
  while IFS='|' read -r _ allow_path _ _; do
    [ -n "$allow_path" ] || continue
    if ! grep -Fqx "$allow_path" "$work/seen.list"; then
      echo "vendor drift check failed: allowlisted $allow_path no longer differs from upstream (patch lost or allowlist stale)" >&2
      failed=1
    fi
  done <"$allow_file"

  rm -rf "$work"
  if [ "$failed" -ne 0 ]; then
    return 1
  fi
  echo "vendor tree matches upstream except $allowed allowlisted paths"
}

read_exact_pins() {
  grep -o 'llama-cpp-2 = { version = "=[0-9][0-9.]*"' "$@" | sed 's/.*"=//; s/"$//'
}

check_version_pins() {
  model_client_toml="$1"
  spike_toml="$2"
  vendor_toml="$3"
  expected="$4"

  pins="$(read_exact_pins "$model_client_toml" "$spike_toml" || true)"
  pin_count="$(printf '%s\n' "$pins" | sed '/^$/d' | wc -l | tr -d ' ')"
  if [ "$pin_count" -ne 3 ]; then
    echo "vendor drift check failed: expected 3 exact llama-cpp-2 pins (2 in model_client, 1 in tools/spike), found $pin_count" >&2
    return 1
  fi
  stray="$(printf '%s\n' "$pins" | grep -v "^${expected}\$" || true)"
  if [ -n "$stray" ]; then
    echo "vendor drift check failed: llama-cpp-2 is pinned to $(printf '%s' "$stray" | tr '\n' ' ')but this checker expects $expected" >&2
    return 1
  fi

  vendored="$(sed -n 's/^version = "\([0-9][0-9.]*\)"$/\1/p' "$vendor_toml" | head -1)"
  if [ "$vendored" != "$expected" ]; then
    echo "vendor drift check failed: vendored manifest is version ${vendored:-<none>}, the pins say $expected" >&2
    return 1
  fi
  echo "llama-cpp-2 pins agree on $expected (2 model_client + 1 spike + vendored manifest)"
}

# Print the path of a local upstream .crate tarball, or nothing.
locate_cached_crate() {
  version="$1"
  if [ -n "${COMPME_VENDOR_CRATE_PATH:-}" ]; then
    if [ -f "$COMPME_VENDOR_CRATE_PATH" ]; then
      printf '%s\n' "$COMPME_VENDOR_CRATE_PATH"
    fi
    return 0
  fi
  for root in "${CARGO_HOME:-}" "$repo_root/target/.cargo" "$HOME/.cargo"; do
    [ -n "$root" ] || continue
    for candidate in "$root"/registry/cache/*/"llama-cpp-2-$version.crate"; do
      if [ -f "$candidate" ]; then
        printf '%s\n' "$candidate"
        return 0
      fi
    done
  done
}

verify_crate_sha() {
  crate_file="$1"
  crate_sha="$(sha256_of "$crate_file")"
  if [ "$crate_sha" != "$upstream_sha256" ]; then
    echo "vendor drift check failed: $crate_file has sha256 $crate_sha, expected $upstream_sha256 for llama-cpp-2 $expected_version" >&2
    return 1
  fi
}

# Unpack a .crate tarball and print the crate directory inside it.
extract_crate() {
  ex_crate="$1"
  ex_version="$2"
  ex_dest="$3"
  mkdir -p "$ex_dest"
  if ! tar xzf "$ex_crate" -C "$ex_dest" 2>/dev/null; then
    echo "vendor drift check failed: $ex_crate is not a readable .crate tarball" >&2
    return 1
  fi
  if [ ! -d "$ex_dest/llama-cpp-2-$ex_version" ]; then
    echo "vendor drift check failed: $ex_crate does not contain llama-cpp-2-$ex_version/" >&2
    return 1
  fi
  printf '%s\n' "$ex_dest/llama-cpp-2-$ex_version"
}

# The stronger assurance level: the committed digests really are the published
# tarball's, so a hand-doctored manifest cannot launder drift.
assert_manifest_reproduces() {
  ar_entries="$1"
  ar_crate="$2"
  ar_version="$3"
  ar_work="$4"
  ar_dir="$(extract_crate "$ar_crate" "$ar_version" "$ar_work/upstream")" || return 1
  digest_tree "$ar_dir" >"$ar_work/fresh-entries.txt"
  if ! diff -u "$ar_entries" "$ar_work/fresh-entries.txt" >"$ar_work/manifest.diff" 2>&1; then
    echo "vendor drift check failed: the committed digest manifest does not re-derive from $ar_crate; regenerate it with --write-manifest" >&2
    sed -n '1,20p' "$ar_work/manifest.diff" >&2
    return 1
  fi
}

write_manifest() {
  wm_crate="$1"
  wm_out="$2"
  wm_work="$3"
  wm_dir="$(extract_crate "$wm_crate" "$expected_version" "$wm_work/upstream")" || return 1
  {
    echo "# Upstream file digests for llama-cpp-2 $expected_version — the ground truth"
    echo "# for tools/release/check-vendor-drift.sh."
    echo "#"
    echo "# Generated by: tools/release/check-vendor-drift.sh --write-manifest"
    echo "# version: $expected_version"
    echo "# tarball-sha256: $upstream_sha256"
    echo "#"
    echo "# One line per file in the published .crate, as \"<sha256>  <path>\","
    echo "# LC_ALL=C sorted, excluding cargo's .cargo-ok extraction marker. Committed"
    echo "# so the drift check verifies vendor/llama-cpp-2 offline, with no registry"
    echo "# cache and no network: both lockfiles patch llama-cpp-2 to vendor/, so a CI"
    echo "# runner never downloads the tarball. Re-stamp it with --write-manifest in"
    echo "# the same commit as any llama-cpp-2 bump."
    digest_tree "$wm_dir"
  } >"$wm_out"
}

run_self_test() {
  for name in COMPME_VENDOR_CRATE_PATH; do
    if printenv "$name" >/dev/null 2>&1; then
      echo "vendor drift self-test failed: inherited $name" >&2
      return 1
    fi
  done
  unset COMPME_VENDOR_CRATE_PATH

  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-vendor-drift.XXXXXX")"
  # shellcheck disable=SC2064
  trap "rm -rf '$tmp'" RETURN
  script="$repo_root/tools/release/check-vendor-drift.sh"

  up="$tmp/upstream"
  vd="$tmp/vendor"
  mkdir -p "$up/src" "$vd/src"
  printf 'shared\n' >"$up/src/keep.rs"
  printf 'shared\n' >"$vd/src/keep.rs"
  printf 'upstream\n' >"$up/src/patched.rs"
  printf 'patched\n' >"$vd/src/patched.rs"
  printf 'license\n' >"$vd/LICENSE-MIT"
  printf '{"v":1}\n' >"$vd/.cargo-ok"

  # The fixture's "upstream" is a digest list, exactly like the committed
  # manifest body: no upstream tree is consulted during a comparison.
  digest_tree "$up" >"$tmp/entries.txt"

  patched_sha="$(sha256_of "$vd/src/patched.rs")"
  license_sha="$(sha256_of "$vd/LICENSE-MIT")"
  {
    printf 'patched|src/patched.rs|%s|self-test patch\n' "$patched_sha"
    printf 'added|LICENSE-MIT|%s|self-test addition\n' "$license_sha"
  } >"$tmp/allow.txt"

  # Clean tree: the allowlisted patch and addition pass, `.cargo-ok` is ignored.
  compare_manifest "$tmp/entries.txt" "$vd" "$tmp/allow.txt" >"$tmp/clean.out"
  grep -q '^vendor tree matches upstream except 2 allowlisted paths$' "$tmp/clean.out"

  # A vendored file whose digest does not match the manifest.
  printf 'sneaky\n' >"$vd/src/keep.rs"
  if compare_manifest "$tmp/entries.txt" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/modified.err"; then
    echo "vendor drift self-test failed: an unallowlisted modified file was accepted" >&2
    return 1
  fi
  grep -q 'unexpected patched file src/keep\.rs is not on the allowlist' "$tmp/modified.err"
  printf 'shared\n' >"$vd/src/keep.rs"

  # Injected drift inside an allowlisted file: the digest must catch it.
  printf 'patched plus a smuggled line\n' >"$vd/src/patched.rs"
  if compare_manifest "$tmp/entries.txt" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/digest.err"; then
    echo "vendor drift self-test failed: a changed allowlisted file was accepted" >&2
    return 1
  fi
  grep -q "allowlisted src/patched\.rs changed: expected sha256 $patched_sha" "$tmp/digest.err"
  printf 'patched\n' >"$vd/src/patched.rs"

  # A file added to the vendored tree without an allowlist entry.
  printf 'extra\n' >"$vd/src/extra.rs"
  if compare_manifest "$tmp/entries.txt" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/added.err"; then
    echo "vendor drift self-test failed: an unallowlisted added file was accepted" >&2
    return 1
  fi
  grep -q 'unexpected added file src/extra\.rs is not on the allowlist' "$tmp/added.err"
  rm -f "$vd/src/extra.rs"

  # A file dropped from the vendored tree.
  rm -f "$vd/src/keep.rs"
  if compare_manifest "$tmp/entries.txt" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/removed.err"; then
    echo "vendor drift self-test failed: a file missing from the vendored tree was accepted" >&2
    return 1
  fi
  grep -q 'src/keep\.rs is missing from the vendored tree' "$tmp/removed.err"
  printf 'shared\n' >"$vd/src/keep.rs"

  # The patch silently reverting to upstream is drift, not a pass.
  printf 'upstream\n' >"$vd/src/patched.rs"
  if compare_manifest "$tmp/entries.txt" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/lost.err"; then
    echo "vendor drift self-test failed: a lost patch was accepted" >&2
    return 1
  fi
  grep -q 'allowlisted src/patched\.rs no longer differs from upstream' "$tmp/lost.err"
  printf 'patched\n' >"$vd/src/patched.rs"

  # An allowlist entry filed under the wrong kind.
  sed 's/^patched|src\/patched.rs/added|src\/patched.rs/' "$tmp/allow.txt" >"$tmp/allow-kind.txt"
  if compare_manifest "$tmp/entries.txt" "$vd" "$tmp/allow-kind.txt" >/dev/null 2>"$tmp/kind.err"; then
    echo "vendor drift self-test failed: a mis-filed allowlist kind was accepted" >&2
    return 1
  fi
  grep -q 'allowlist records src/patched\.rs as added, the tree shows patched' "$tmp/kind.err"

  # Manifest validation: missing, and every shape of malformed.
  if read_manifest "$tmp/absent.sha256" "$tmp/entries-out.txt" 2>"$tmp/manifest-missing.err"; then
    echo "vendor drift self-test failed: a missing digest manifest was accepted" >&2
    return 1
  fi
  grep -q 'digest manifest .*absent\.sha256 is missing' "$tmp/manifest-missing.err"

  good_manifest="$tmp/manifest.sha256"
  {
    printf '# version: %s\n' "$expected_version"
    printf '# tarball-sha256: %s\n' "$upstream_sha256"
    printf '\n'
    cat "$tmp/entries.txt"
  } >"$good_manifest"
  read_manifest "$good_manifest" "$tmp/entries-out.txt"
  if ! cmp -s "$tmp/entries.txt" "$tmp/entries-out.txt"; then
    echo "vendor drift self-test failed: a well-formed manifest did not round-trip" >&2
    return 1
  fi

  sed "s/^# version: .*/# version: 9.9.9/" "$good_manifest" >"$tmp/manifest-version.sha256"
  if read_manifest "$tmp/manifest-version.sha256" "$tmp/entries-out.txt" 2>"$tmp/manifest-version.err"; then
    echo "vendor drift self-test failed: a manifest with the wrong version header was accepted" >&2
    return 1
  fi
  grep -q "header records version 9\.9\.9, this checker expects $expected_version" "$tmp/manifest-version.err"

  grep -v '^# tarball-sha256: ' "$good_manifest" >"$tmp/manifest-nosha.sha256"
  if read_manifest "$tmp/manifest-nosha.sha256" "$tmp/entries-out.txt" 2>"$tmp/manifest-nosha.err"; then
    echo "vendor drift self-test failed: a manifest with no tarball-sha256 header was accepted" >&2
    return 1
  fi
  grep -q 'header records tarball-sha256 <none>' "$tmp/manifest-nosha.err"

  grep '^#' "$good_manifest" >"$tmp/manifest-empty.sha256"
  if read_manifest "$tmp/manifest-empty.sha256" "$tmp/entries-out.txt" 2>"$tmp/manifest-empty.err"; then
    echo "vendor drift self-test failed: a manifest with no digests was accepted" >&2
    return 1
  fi
  grep -q 'it records no file digests' "$tmp/manifest-empty.err"

  sed 's|^[0-9a-f]\{64\}  src/keep.rs$|deadbeef src/keep.rs|' "$good_manifest" >"$tmp/manifest-shape.sha256"
  if read_manifest "$tmp/manifest-shape.sha256" "$tmp/entries-out.txt" 2>"$tmp/manifest-shape.err"; then
    echo "vendor drift self-test failed: a malformed manifest entry was accepted" >&2
    return 1
  fi
  grep -q 'is not "<sha256>  <path>"' "$tmp/manifest-shape.err"

  { cat "$good_manifest"; grep 'src/keep.rs$' "$tmp/entries.txt"; } >"$tmp/manifest-dupe.sha256"
  if read_manifest "$tmp/manifest-dupe.sha256" "$tmp/entries-out.txt" 2>"$tmp/manifest-dupe.err"; then
    echo "vendor drift self-test failed: a manifest with a duplicate path was accepted" >&2
    return 1
  fi
  grep -q 'duplicate path src/keep\.rs' "$tmp/manifest-dupe.err"

  # A manifest that disagrees with the tarball, on a hermetic fake .crate.
  fake_version="9.9.9"
  fake_root="$tmp/fake/llama-cpp-2-$fake_version"
  mkdir -p "$fake_root/src"
  printf 'fake lib\n' >"$fake_root/src/lib.rs"
  printf 'fake manifest\n' >"$fake_root/Cargo.toml"
  (cd "$tmp/fake" && tar czf "$tmp/fake.crate" "llama-cpp-2-$fake_version")
  mkdir -p "$tmp/repro-ok"
  digest_tree "$fake_root" >"$tmp/fake-entries.txt"
  assert_manifest_reproduces "$tmp/fake-entries.txt" "$tmp/fake.crate" "$fake_version" "$tmp/repro-ok"
  sed 's|^[0-9a-f]\{64\}|00000000000000000000000000000000000000000000000000000000000000ff|' \
    "$tmp/fake-entries.txt" >"$tmp/fake-entries-doctored.txt"
  mkdir -p "$tmp/repro-bad"
  if assert_manifest_reproduces "$tmp/fake-entries-doctored.txt" "$tmp/fake.crate" "$fake_version" \
    "$tmp/repro-bad" 2>"$tmp/repro.err"; then
    echo "vendor drift self-test failed: a doctored manifest was accepted against the tarball" >&2
    return 1
  fi
  grep -q 'does not re-derive from .*fake\.crate' "$tmp/repro.err"

  mkdir -p "$tmp/repro-missing"
  if assert_manifest_reproduces "$tmp/fake-entries.txt" "$tmp/fake.crate" "0.0.0" \
    "$tmp/repro-missing" 2>"$tmp/repro-layout.err"; then
    echo "vendor drift self-test failed: a tarball with the wrong crate directory was accepted" >&2
    return 1
  fi
  grep -q 'does not contain llama-cpp-2-0\.0\.0/' "$tmp/repro-layout.err"

  # Version pin agreement across the three Cargo.toml pins and the manifest.
  printf 'llama-cpp-2 = { version = "=9.9.9", default-features = false, features = ["metal"] }\n' >"$tmp/model-client.toml"
  printf 'llama-cpp-2 = { version = "=9.9.9", default-features = false }\n' >>"$tmp/model-client.toml"
  printf 'llama-cpp-2 = { version = "=9.9.9", default-features = false, features = ["metal"] }\n' >"$tmp/spike.toml"
  printf 'version = "9.9.9"\n' >"$tmp/vendor.toml"
  check_version_pins "$tmp/model-client.toml" "$tmp/spike.toml" "$tmp/vendor.toml" 9.9.9 >/dev/null

  head -1 "$tmp/model-client.toml" >"$tmp/model-client-short.toml"
  if check_version_pins "$tmp/model-client-short.toml" "$tmp/spike.toml" "$tmp/vendor.toml" 9.9.9 \
    >/dev/null 2>"$tmp/pin-count.err"; then
    echo "vendor drift self-test failed: a missing exact pin was accepted" >&2
    return 1
  fi
  grep -q 'expected 3 exact llama-cpp-2 pins' "$tmp/pin-count.err"

  sed 's/=9\.9\.9/=9.9.8/' "$tmp/spike.toml" >"$tmp/spike-stale.toml"
  if check_version_pins "$tmp/model-client.toml" "$tmp/spike-stale.toml" "$tmp/vendor.toml" 9.9.9 \
    >/dev/null 2>"$tmp/pin-stale.err"; then
    echo "vendor drift self-test failed: a mismatched exact pin was accepted" >&2
    return 1
  fi
  grep -q 'llama-cpp-2 is pinned to 9\.9\.8' "$tmp/pin-stale.err"

  printf 'version = "9.9.8"\n' >"$tmp/vendor-stale.toml"
  if check_version_pins "$tmp/model-client.toml" "$tmp/spike.toml" "$tmp/vendor-stale.toml" 9.9.9 \
    >/dev/null 2>"$tmp/vendor-stale.err"; then
    echo "vendor drift self-test failed: a stale vendored manifest version was accepted" >&2
    return 1
  fi
  grep -q 'vendored manifest is version 9\.9\.8' "$tmp/vendor-stale.err"

  # The real allowlist must parse into four fields with a non-empty reason.
  allowlist >"$tmp/real-allow.txt"
  if ! awk -F'|' 'NF != 4 || $1 !~ /^(patched|added)$/ || $3 !~ /^[0-9a-f]{64}$/ || $4 == "" { bad = 1 } END { exit bad }' \
    "$tmp/real-allow.txt"; then
    echo "vendor drift self-test failed: the allowlist has a malformed entry" >&2
    return 1
  fi

  # No local tarball must still VERIFY against the committed manifest. This is
  # the CI shape: both lockfiles patch llama-cpp-2 to vendor/, so no runner
  # ever has the .crate.
  COMPME_VENDOR_CRATE_PATH="$tmp/absent.crate" \
    "$script" >"$tmp/offline.out"
  grep -q '^vendor tree matches upstream except ' "$tmp/offline.out"
  grep -q "^no local llama-cpp-2-$expected_version\.crate: " "$tmp/offline.out"
  grep -q '^vendor drift check passed: ' "$tmp/offline.out"

  # ... and a missing manifest must fail that same run, not skip it. Run the
  # script against a stand-in repo root whose inputs are symlinks to the real
  # ones and whose manifest is absent.
  fake_repo="$tmp/fake-repo"
  mkdir -p "$fake_repo/tools/release" "$fake_repo/crates/model_client" "$fake_repo/tools/spike"
  cp "$script" "$fake_repo/tools/release/check-vendor-drift.sh"
  ln -s "$repo_root/vendor" "$fake_repo/vendor"
  ln -s "$repo_root/crates/model_client/Cargo.toml" "$fake_repo/crates/model_client/Cargo.toml"
  ln -s "$repo_root/tools/spike/Cargo.toml" "$fake_repo/tools/spike/Cargo.toml"
  if COMPME_VENDOR_CRATE_PATH="$tmp/absent.crate" \
    "$fake_repo/tools/release/check-vendor-drift.sh" >/dev/null 2>"$tmp/no-manifest.err"; then
    echo "vendor drift self-test failed: a run with no digest manifest was accepted" >&2
    return 1
  fi
  grep -q 'digest manifest .*vendor-llama-cpp-2\.sha256 is missing' "$tmp/no-manifest.err"

  if "$script" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "vendor drift self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-vendor-drift\.sh \[--self-test | --write-manifest\]$' "$tmp/self-test-argc.err"
  if "$script" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "vendor drift self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-vendor-drift\.sh \[--self-test | --write-manifest\]$' "$tmp/normal-argc.err"

  echo "Self-test passed"
}

mode="verify"
if [ "$#" -gt 1 ]; then
  usage
  exit 2
fi
case "${1:-}" in
  "") ;;
  --self-test)
    run_self_test
    exit 0
    ;;
  --write-manifest) mode="write" ;;
  *)
    usage
    exit 2
    ;;
esac

check_version_pins \
  "$repo_root/crates/model_client/Cargo.toml" \
  "$repo_root/tools/spike/Cargo.toml" \
  "$repo_root/vendor/llama-cpp-2/Cargo.toml" \
  "$expected_version"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/compme-vendor-drift-run.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

crate_path="$(locate_cached_crate "$expected_version")"

if [ "$mode" = "write" ]; then
  if [ -z "$crate_path" ]; then
    echo "vendor drift check failed: --write-manifest needs a local llama-cpp-2-$expected_version.crate; run a cargo build against crates.io or set COMPME_VENDOR_CRATE_PATH" >&2
    exit 1
  fi
  verify_crate_sha "$crate_path"
  write_manifest "$crate_path" "$manifest_path" "$scratch"
  echo "wrote $manifest_path from $crate_path ($(grep -c -v -e '^#' -e '^[[:space:]]*$' "$manifest_path") upstream files)"
  exit 0
fi

read_manifest "$manifest_path" "$scratch/entries.txt"
allowlist >"$scratch/allow.txt"
compare_manifest "$scratch/entries.txt" "$repo_root/vendor/llama-cpp-2" "$scratch/allow.txt"

if [ -n "$crate_path" ]; then
  verify_crate_sha "$crate_path"
  assert_manifest_reproduces "$scratch/entries.txt" "$crate_path" "$expected_version" "$scratch"
  echo "digest manifest re-derived from $crate_path: the committed digests are the published tarball's"
else
  echo "no local llama-cpp-2-$expected_version.crate: verified against the committed digest manifest alone (set COMPME_VENDOR_CRATE_PATH to a tarball to also re-derive it)"
fi
echo "vendor drift check passed: vendor/llama-cpp-2 matches llama-cpp-2 $expected_version"
