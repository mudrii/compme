#!/usr/bin/env bash
# Guard `vendor/llama-cpp-2` against silent drift from the pinned upstream
# release. The vendored tree is the crates.io `llama-cpp-2` release plus the
# safe abort-lifetime extension; nothing else may differ. Both the root
# workspace and tools/spike point `[patch.crates-io]` at it, so an unnoticed
# edit here ships into every binary with no compiler or lockfile signal.
#
# Ground truth is the upstream `.crate` tarball (pinned by sha256 below),
# taken from a local cargo registry cache when one has it and fetched only as
# a fallback. Every path that differs from that tarball must appear in the
# allowlist with the digest of the vendored file and the reason it is patched,
# so neither a new difference nor a changed patch can pass as "intentional",
# and a patch that silently disappears fails too.
#
# Bumping llama-cpp-2 means re-stamping expected_version, upstream_sha256, and
# every allowlist digest in the same commit as the vendored rebase.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$repo_root"

# Pinned upstream release. Cross-checked against the three `=x.y.z` Cargo.toml
# pins and the vendored manifest, so a bump cannot land in only some of them.
expected_version="0.1.146"
# sha256 of https://static.crates.io/crates/llama-cpp-2/llama-cpp-2-0.1.146.crate
upstream_sha256="f3b0f368c76cc0fe475e8257aeeec269e0d6569bd48b1f503efd0963fc3ee397"

usage() {
  echo "usage: check-vendor-drift.sh [--self-test]" >&2
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
# sides keeps the comparison identical whichever upstream source is used.
list_tree() {
  dir="$1"
  (cd "$dir" && find . -type f ! -name .cargo-ok | sed 's|^\./||' | LC_ALL=C sort)
}

# Compare a vendored tree against an upstream tree, allowing only the paths in
# the allowlist file (same line format as `allowlist`).
compare_trees() {
  upstream="$1"
  vendor="$2"
  allow_file="$3"
  work="$(mktemp -d "${TMPDIR:-/tmp}/compme-vendor-drift-cmp.XXXXXX")"

  list_tree "$upstream" >"$work/upstream.list"
  list_tree "$vendor" >"$work/vendor.list"
  LC_ALL=C sort -u "$work/upstream.list" "$work/vendor.list" >"$work/union.list"
  : >"$work/seen.list"

  failed=0
  allowed=0
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    if [ -f "$upstream/$path" ] && [ -f "$vendor/$path" ]; then
      if cmp -s "$upstream/$path" "$vendor/$path"; then
        continue
      fi
      kind="patched"
    elif [ -f "$vendor/$path" ]; then
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
    actual_sha="$(sha256_of "$vendor/$path")"
    if [ "$actual_sha" != "$allow_sha" ]; then
      echo "vendor drift check failed: allowlisted $path changed: expected sha256 $allow_sha, found $actual_sha" >&2
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

fetch_crate() {
  version="$1"
  dest="$2"
  if [ -n "${COMPME_VENDOR_DRIFT_OFFLINE:-}" ]; then
    return 1
  fi
  command -v curl >/dev/null 2>&1 || return 1
  curl -fsSL --max-time 60 \
    -o "$dest" \
    "https://static.crates.io/crates/llama-cpp-2/llama-cpp-2-$version.crate" 2>/dev/null
}

run_self_test() {
  for name in COMPME_VENDOR_CRATE_PATH COMPME_VENDOR_DRIFT_OFFLINE; do
    if printenv "$name" >/dev/null 2>&1; then
      echo "vendor drift self-test failed: inherited $name" >&2
      return 1
    fi
  done
  unset COMPME_VENDOR_CRATE_PATH COMPME_VENDOR_DRIFT_OFFLINE

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

  patched_sha="$(sha256_of "$vd/src/patched.rs")"
  license_sha="$(sha256_of "$vd/LICENSE-MIT")"
  {
    printf 'patched|src/patched.rs|%s|self-test patch\n' "$patched_sha"
    printf 'added|LICENSE-MIT|%s|self-test addition\n' "$license_sha"
  } >"$tmp/allow.txt"

  # Clean tree: the allowlisted patch and addition pass, `.cargo-ok` is ignored.
  compare_trees "$up" "$vd" "$tmp/allow.txt" >"$tmp/clean.out"
  grep -q '^vendor tree matches upstream except 2 allowlisted paths$' "$tmp/clean.out"

  # Injected drift in a file nobody patched.
  printf 'sneaky\n' >"$vd/src/keep.rs"
  if compare_trees "$up" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/modified.err"; then
    echo "vendor drift self-test failed: an unallowlisted modified file was accepted" >&2
    return 1
  fi
  grep -q 'unexpected patched file src/keep\.rs is not on the allowlist' "$tmp/modified.err"
  printf 'shared\n' >"$vd/src/keep.rs"

  # Injected drift inside an allowlisted file: the digest must catch it.
  printf 'patched plus a smuggled line\n' >"$vd/src/patched.rs"
  if compare_trees "$up" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/digest.err"; then
    echo "vendor drift self-test failed: a changed allowlisted file was accepted" >&2
    return 1
  fi
  grep -q "allowlisted src/patched\.rs changed: expected sha256 $patched_sha" "$tmp/digest.err"
  printf 'patched\n' >"$vd/src/patched.rs"

  # A file added to the vendored tree without an allowlist entry.
  printf 'extra\n' >"$vd/src/extra.rs"
  if compare_trees "$up" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/added.err"; then
    echo "vendor drift self-test failed: an unallowlisted added file was accepted" >&2
    return 1
  fi
  grep -q 'unexpected added file src/extra\.rs is not on the allowlist' "$tmp/added.err"
  rm -f "$vd/src/extra.rs"

  # A file dropped from the vendored tree.
  rm -f "$vd/src/keep.rs"
  if compare_trees "$up" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/removed.err"; then
    echo "vendor drift self-test failed: a file missing from the vendored tree was accepted" >&2
    return 1
  fi
  grep -q 'src/keep\.rs is missing from the vendored tree' "$tmp/removed.err"
  printf 'shared\n' >"$vd/src/keep.rs"

  # The patch silently reverting to upstream is drift, not a pass.
  printf 'upstream\n' >"$vd/src/patched.rs"
  if compare_trees "$up" "$vd" "$tmp/allow.txt" >/dev/null 2>"$tmp/lost.err"; then
    echo "vendor drift self-test failed: a lost patch was accepted" >&2
    return 1
  fi
  grep -q 'allowlisted src/patched\.rs no longer differs from upstream' "$tmp/lost.err"
  printf 'patched\n' >"$vd/src/patched.rs"

  # An allowlist entry filed under the wrong kind.
  sed 's/^patched|src\/patched.rs/added|src\/patched.rs/' "$tmp/allow.txt" >"$tmp/allow-kind.txt"
  if compare_trees "$up" "$vd" "$tmp/allow-kind.txt" >/dev/null 2>"$tmp/kind.err"; then
    echo "vendor drift self-test failed: a mis-filed allowlist kind was accepted" >&2
    return 1
  fi
  grep -q 'allowlist records src/patched\.rs as added, the tree shows patched' "$tmp/kind.err"

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

  # Offline with no cached tarball degrades to a skip, not a gate failure.
  COMPME_VENDOR_DRIFT_OFFLINE=1 COMPME_VENDOR_CRATE_PATH="$tmp/absent.crate" \
    "$script" >"$tmp/offline.out"
  grep -q '^vendor drift check skipped: ' "$tmp/offline.out"

  if "$script" --self-test unexpected-extra >/dev/null 2>"$tmp/self-test-argc.err"; then
    echo "vendor drift self-test failed: extra --self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-vendor-drift\.sh \[--self-test\]$' "$tmp/self-test-argc.err"
  if "$script" unexpected-extra >/dev/null 2>"$tmp/normal-argc.err"; then
    echo "vendor drift self-test failed: extra normal argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: check-vendor-drift\.sh \[--self-test\]$' "$tmp/normal-argc.err"

  echo "Self-test passed"
}

if [ "${1:-}" = "--self-test" ]; then
  if [ "$#" -ne 1 ]; then
    usage
    exit 2
  fi
  run_self_test
  exit 0
fi
if [ "$#" -ne 0 ]; then
  usage
  exit 2
fi

check_version_pins \
  "$repo_root/crates/model_client/Cargo.toml" \
  "$repo_root/tools/spike/Cargo.toml" \
  "$repo_root/vendor/llama-cpp-2/Cargo.toml" \
  "$expected_version"

scratch="$(mktemp -d "${TMPDIR:-/tmp}/compme-vendor-drift-run.XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

crate_path="$(locate_cached_crate "$expected_version")"
crate_source="the cargo registry cache"
if [ -z "$crate_path" ]; then
  if fetch_crate "$expected_version" "$scratch/upstream.crate"; then
    crate_path="$scratch/upstream.crate"
    crate_source="static.crates.io"
  else
    echo "vendor drift check skipped: no local llama-cpp-2-$expected_version.crate and it could not be fetched; run a cargo build (or set COMPME_VENDOR_CRATE_PATH) to populate the registry cache"
    exit 0
  fi
fi

crate_sha="$(sha256_of "$crate_path")"
if [ "$crate_sha" != "$upstream_sha256" ]; then
  echo "vendor drift check failed: $crate_path has sha256 $crate_sha, expected $upstream_sha256 for llama-cpp-2 $expected_version" >&2
  exit 1
fi

mkdir -p "$scratch/upstream"
tar xzf "$crate_path" -C "$scratch/upstream"
upstream_dir="$scratch/upstream/llama-cpp-2-$expected_version"
if [ ! -d "$upstream_dir" ]; then
  echo "vendor drift check failed: $crate_path does not contain llama-cpp-2-$expected_version/" >&2
  exit 1
fi

allowlist >"$scratch/allow.txt"
compare_trees "$upstream_dir" "$repo_root/vendor/llama-cpp-2" "$scratch/allow.txt" >"$scratch/compare.out"
cat "$scratch/compare.out"
echo "vendor drift check passed: vendor/llama-cpp-2 matches llama-cpp-2 $expected_version from $crate_source"
