#!/usr/bin/env bash
# Build the existing strict latency test once, then run that exact artifact in
# fresh processes. Exit 0 means every sample met the 500 ms budget, exit 1
# means the measurements completed but at least one missed it, and exit 2
# means the benchmark itself was not trustworthy.
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/../.." && pwd)"
budget_ms=500
context_tokens=256
test_name="warm_completion_under_500ms"
default_model="$repo_root/tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf"

usage() {
  echo "usage: $0 --backend cpu|vulkan --samples N --output DIR | --self-test" >&2
}

fail() {
  echo "benchmark-model: $*" >&2
  exit 2
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | awk '{print $1}'
  else
    return 1
  fi
}

write_report() {
  local overall_status="$1"
  local overall_exit="$2"
  local error_text="$3"
  local observed_backend="$4"
  REPORT_GENERATED_AT="$(date -u '+%Y-%m-%dT%H:%M:%SZ')" \
    REPORT_STATUS="$overall_status" REPORT_EXIT="$overall_exit" \
    REPORT_ERROR="$error_text" REPORT_GIT_REV="$git_revision" \
    REPORT_GIT_DIRTY="$git_dirty" REPORT_MODEL_PATH="$model_path" \
    REPORT_MODEL_BYTES="$model_bytes" REPORT_MODEL_SHA256="$model_sha256" \
    REPORT_BACKEND="$backend" REPORT_OBSERVED="$observed_backend" \
    REPORT_FEATURE="$cargo_feature" REPORT_GPU_LAYERS="$gpu_layers" \
    REPORT_TEST="$test_name" REPORT_SAMPLES="$samples" \
    REPORT_CONTEXT="$context_tokens" REPORT_BUDGET="$budget_ms" \
    REPORT_CARGO_VERSION="$cargo_version" REPORT_RUSTC_VERSION="$rustc_version" \
    REPORT_HOST_UNAME="$host_uname" REPORT_CPU_MODEL="$cpu_model" \
    ruby -rjson -e '
      report_path, samples_path, artifact_path = ARGV.shift(3)
      samples = []
      if File.file?(samples_path)
        File.readlines(samples_path, chomp: true).drop(1).each do |line|
          fields = line.split("\t", -1)
          next if fields.empty? || fields[0].empty?
          samples << {
            "index" => Integer(fields[0]),
            "elapsed_ms" => fields[1].empty? ? nil : Integer(fields[1]),
            "outcome" => fields[2],
            "process_exit" => Integer(fields[3]),
            "observed_backend" => fields[4].empty? ? nil : fields[4],
            "offloaded_layers" => fields[5].empty? ? nil : Integer(fields[5]),
            "total_layers" => fields[6].empty? ? nil : Integer(fields[6]),
            "log" => fields[7],
            "error" => fields[8].empty? ? nil : fields[8]
          }
        end
      end
      elapsed = samples.select { |sample| sample["outcome"] != "error" }
        .map { |sample| sample["elapsed_ms"] }.compact.sort
      median = if elapsed.empty?
        nil
      elsif elapsed.length.odd?
        elapsed[elapsed.length / 2]
      else
        (elapsed[elapsed.length / 2 - 1] + elapsed[elapsed.length / 2]) / 2.0
      end
      artifact = File.file?(artifact_path) ? JSON.parse(File.read(artifact_path)) : nil
      env_keys = %w[
        RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_TARGET_DIR CARGO_BUILD_JOBS
        CARGO_INCREMENTAL CARGO_PROFILE_DEV_OPT_LEVEL CARGO_PROFILE_TEST_OPT_LEVEL
        CARGO_PROFILE_TEST_DEBUG CARGO_PROFILE_TEST_DEBUG_ASSERTIONS
        CARGO_PROFILE_TEST_OVERFLOW_CHECKS CMAKE_BUILD_TYPE CMAKE_ARGS GGML_NATIVE
        VK_DRIVER_FILES VK_ICD_FILENAMES LD_LIBRARY_PATH
      ]
      build_environment = env_keys.to_h { |key| [key, ENV[key]] }.compact
      report = {
        "schema_version" => 1,
        "generated_at_utc" => ENV.fetch("REPORT_GENERATED_AT"),
        "status" => ENV.fetch("REPORT_STATUS"),
        "exit_status" => Integer(ENV.fetch("REPORT_EXIT")),
        "error" => ENV.fetch("REPORT_ERROR", "").empty? ? nil : ENV["REPORT_ERROR"],
        "git" => {
          "revision" => ENV.fetch("REPORT_GIT_REV"),
        "dirty" => ENV.fetch("REPORT_GIT_DIRTY") == "true"
      },
      "host" => {
        "uname" => ENV.fetch("REPORT_HOST_UNAME"),
        "cpu_model" => ENV.fetch("REPORT_CPU_MODEL")
      },
        "model" => {
          "path" => ENV.fetch("REPORT_MODEL_PATH"),
          "bytes" => Integer(ENV.fetch("REPORT_MODEL_BYTES")),
          "sha256" => ENV.fetch("REPORT_MODEL_SHA256")
        },
        "backend" => {
          "requested" => ENV.fetch("REPORT_BACKEND"),
          "observed" => ENV.fetch("REPORT_OBSERVED", "").empty? ? nil : ENV["REPORT_OBSERVED"],
          "cargo_feature" => ENV.fetch("REPORT_FEATURE", "").empty? ? nil : ENV["REPORT_FEATURE"],
          "gpu_layers" => Integer(ENV.fetch("REPORT_GPU_LAYERS"))
        },
        "configuration" => {
          "test" => ENV.fetch("REPORT_TEST"),
          "profile" => "test",
          "samples_requested" => Integer(ENV.fetch("REPORT_SAMPLES")),
          "context_tokens" => Integer(ENV.fetch("REPORT_CONTEXT")),
          "budget_ms_exclusive" => Integer(ENV.fetch("REPORT_BUDGET")),
          "build_environment" => build_environment
        },
        "toolchain" => {
          "cargo" => ENV.fetch("REPORT_CARGO_VERSION"),
          "rustc" => ENV.fetch("REPORT_RUSTC_VERSION")
        },
        "build_artifact" => artifact,
        "samples" => samples,
        "median_ms" => median,
        "budget_failed_samples" => samples.count { |sample| sample["outcome"] == "budget_failed" }
      }
      File.write(report_path, JSON.pretty_generate(report) + "\n")
    ' "$output_dir/report.json" "$samples_file" "$artifact_meta"
}

analyze_sample() {
  ruby -e '
    path, status_text, requested = ARGV
    text = File.binread(path).encode("UTF-8", invalid: :replace, undef: :replace)
    status = Integer(status_text)
    warm = text.scan(/^warm: ([0-9]+)ms -> /).flatten.map(&:to_i)
    summaries = text.scan(/^test result: (ok|FAILED)\. ([0-9]+) passed; ([0-9]+) failed; ([0-9]+) ignored;/)
    error = nil
    outcome = "error"
    elapsed = warm.length == 1 ? warm[0] : nil
    if warm.length != 1
      error = "expected exactly one warm latency line, found #{warm.length}"
    elsif summaries.length != 1
      error = "expected exactly one test summary, found #{summaries.length}"
    else
      result, passed, failed, ignored = summaries[0]
      if status == 0 && result == "ok" && passed == "1" && failed == "0" && ignored == "0" && elapsed < 500
        outcome = "passed"
      elsif status == 101 && result == "FAILED" && passed == "0" && failed == "1" && ignored == "0"
        threshold = text.scan(/^warm completion ([0-9]+)ms exceeded 500ms$/).flatten.map(&:to_i)
        panics = text.scan(/ panicked at /).length
        if threshold == [elapsed] && panics == 1 && elapsed >= 500
          outcome = "budget_failed"
        else
          error = "test failed for a reason other than the exact 500 ms budget assertion"
        end
      else
        error = "unexpected process exit or test summary"
      end
    end
    observed = requested == "cpu" ? "cpu" : nil
    offloaded = nil
    total = nil
    if requested == "vulkan"
      device = text.match(/using device (Vulkan[0-9]+) /)
      layers = text.match(/offloaded ([0-9]+)\/([0-9]+) layers to GPU/)
      if device && layers && layers[1].to_i > 0 && layers[2].to_i >= layers[1].to_i
        observed = "vulkan"
        offloaded = layers[1].to_i
        total = layers[2].to_i
      elsif error.nil?
        outcome = "error"
        error = "requested Vulkan but native output did not prove positive Vulkan layer offload"
      end
    end
    clean = ->(value) { value.to_s.gsub(/[\t\r\n]/, " ") }
    puts [elapsed, outcome, status, observed, offloaded, total, clean.call(error)].map { |value| clean.call(value) }.join("\u001f")
  ' "$1" "$2" "$backend"
}

run_benchmark() {
  [[ "$(uname -s)" == "Linux" ]] || fail "real benchmarks are supported on Linux only"
  command -v ruby >/dev/null 2>&1 || fail "ruby is required for strict JSON parsing and reports"
  command -v cargo >/dev/null 2>&1 || fail "cargo not found"
  command -v rustc >/dev/null 2>&1 || fail "rustc not found"
  command -v git >/dev/null 2>&1 || fail "git not found"
  [[ -s "$model_path" ]] || fail "model missing or empty: $model_path"
  [[ ! -e "$output_dir" ]] || fail "output path already exists: $output_dir"

  git_revision="$(git -C "$repo_root" rev-parse HEAD)" || fail "could not read git revision"
  local git_status
  git_status="$(git -C "$repo_root" status --porcelain --untracked-files=normal)" || \
    fail "could not read git status"
  if [[ -n "$git_status" ]]; then
    git_dirty=true
  else
    git_dirty=false
  fi
  model_bytes="$(wc -c <"$model_path" | tr -d '[:space:]')"
  model_sha256="$(sha256_file "$model_path")" || fail "sha256sum or shasum is required"
  cargo_version="$(cargo --version)" || fail "could not read cargo version"
  rustc_version="$(rustc --version)" || fail "could not read rustc version"
  host_uname="$(uname -srm)" || fail "could not read host identity"
  cpu_model="$(awk -F ': ' '/^(model name|Hardware|Processor)[[:space:]]*:/{print $2; exit}' /proc/cpuinfo 2>/dev/null || true)"
  [[ -n "$cpu_model" ]] || cpu_model="unavailable"

  mkdir -p "$(dirname "$output_dir")" || fail "could not create output parent: $output_dir"
  mkdir "$output_dir" || fail "could not create fresh output directory: $output_dir"
  output_dir="$(cd "$output_dir" && pwd)"
  samples_file="$output_dir/samples.tsv"
  artifact_meta="$output_dir/artifact.json"
  printf 'index\telapsed_ms\toutcome\tprocess_exit\tobserved_backend\toffloaded_layers\ttotal_layers\tlog\terror\n' >"$samples_file"

  local -a cargo_args=(test --locked -p model_client --test latency --no-run --message-format=json-render-diagnostics)
  cargo_feature=""
  if [[ "$backend" == "vulkan" ]]; then
    cargo_args+=(--features vulkan)
    cargo_feature="vulkan"
    gpu_layers=999
  else
    gpu_layers=0
  fi

  set +e
  (cd "$repo_root" && cargo "${cargo_args[@]}") >"$output_dir/build.jsonl" 2>"$output_dir/build.stderr"
  local build_status=$?
  set -e
  if [[ "$build_status" -ne 0 ]]; then
    if ! write_report "error" 2 "cargo build failed with exit $build_status" ""; then
      echo "benchmark-model: could not write failure report" >&2
    fi
    echo "benchmark-model: build failed; report: $output_dir/report.json" >&2
    return 2
  fi

  local executable
  set +e
  executable="$(ruby -rjson -e '
    input, metadata = ARGV
    messages = File.readlines(input, chomp: true).reject(&:empty?).map { |line| JSON.parse(line) }
    matches = messages.select do |message|
      message["reason"] == "compiler-artifact" &&
        message.dig("target", "name") == "latency" &&
        message.dig("target", "test") == true &&
        message["executable"].is_a?(String)
    end
    abort "expected one latency test artifact, found #{matches.length}" unless matches.length == 1
    artifact = matches[0]
    reduced = {
      "executable" => artifact.fetch("executable"),
      "features" => artifact.fetch("features", []),
      "fresh" => artifact["fresh"],
      "profile" => artifact.fetch("profile")
    }
    File.write(metadata, JSON.pretty_generate(reduced) + "\n")
    puts artifact.fetch("executable")
  ' "$output_dir/build.jsonl" "$artifact_meta" 2>"$output_dir/artifact.stderr")"
  local artifact_status=$?
  set -e
  if [[ "$artifact_status" -ne 0 || ! -x "$executable" ]]; then
    if ! write_report "error" 2 "cargo output did not identify one executable latency test artifact" ""; then
      echo "benchmark-model: could not write failure report" >&2
    fi
    echo "benchmark-model: artifact discovery failed; report: $output_dir/report.json" >&2
    return 2
  fi

  local index log_path process_status analysis elapsed outcome observed offloaded total error
  local operational_failures=0
  local budget_failures=0
  local all_observed="$backend"
  for ((index = 1; index <= samples; index++)); do
    log_path="$output_dir/sample-$(printf '%03d' "$index").log"
    set +e
    COMPME_REQUIRE_LATENCY_BUDGET=1 \
      COMPME_REQUIRE_MODEL_TESTS=1 \
      COMPME_REQUIRE_MODEL_CONTEXT=1 \
      COMPME_MODEL_GPU_LAYERS="$gpu_layers" \
      COMPME_MODEL_CONTEXT_TOKENS="$context_tokens" \
      "$executable" "$test_name" --ignored --exact --nocapture --test-threads=1 \
      >"$log_path" 2>&1
    process_status=$?
    set -e
    set +e
    analysis="$(analyze_sample "$log_path" "$process_status")"
    local analysis_status=$?
    set -e
    if [[ "$analysis_status" -ne 0 ]]; then
      elapsed=""
      outcome="error"
      observed=""
      offloaded=""
      total=""
      error="could not parse sample output"
    else
      IFS=$'\037' read -r elapsed outcome process_status observed offloaded total error <<<"$analysis"
    fi
    if [[ "$outcome" == "error" ]]; then
      operational_failures=$((operational_failures + 1))
      all_observed=""
    elif [[ "$outcome" == "budget_failed" ]]; then
      budget_failures=$((budget_failures + 1))
    fi
    if [[ "$observed" != "$backend" ]]; then
      all_observed=""
    fi
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$index" "$elapsed" "$outcome" "$process_status" "$observed" \
      "$offloaded" "$total" "$(basename "$log_path")" "$error" >>"$samples_file"
  done

  local status exit_status error_text
  if [[ "$operational_failures" -gt 0 ]]; then
    status="error"
    exit_status=2
    error_text="$operational_failures sample(s) were operationally invalid"
  elif [[ "$budget_failures" -gt 0 ]]; then
    status="budget_failed"
    exit_status=1
    error_text="$budget_failures of $samples sample(s) missed the exclusive 500 ms budget"
  else
    status="passed"
    exit_status=0
    error_text=""
  fi
  if ! write_report "$status" "$exit_status" "$error_text" "$all_observed"; then
    echo "benchmark-model: could not write report" >&2
    return 2
  fi
  echo "benchmark-model: $status; report: $output_dir/report.json"
  return "$exit_status"
}

run_self_test() {
  local inherited
  for inherited in COMPME_BENCH_FIXTURE_MODE \
    COMPME_BENCH_FIXTURE_TEST_EXE COMPME_BENCH_FIXTURE_CARGO_LOG \
    COMPME_BENCH_FIXTURE_TEST_LOG; do
    if printenv "$inherited" >/dev/null 2>&1; then
      echo "benchmark-model self-test failed: inherited $inherited" >&2
      return 1
    fi
  done
  # Not local: the EXIT trap expands $tmp after run_self_test returns.
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/compme-benchmark-model.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  local fake_bin="$tmp/bin"
  mkdir -p "$fake_bin"
  local tool path
  for tool in env bash cp dirname mkdir ruby wc tr awk date basename; do
    path="$(command -v "$tool" || true)"
    [[ -n "$path" ]] || {
      echo "benchmark-model self-test failed: required host utility missing: $tool" >&2
      return 1
    }
    ln -s "$path" "$fake_bin/$tool"
  done
  local sha_tool
  sha_tool="$(command -v sha256sum || command -v shasum || true)"
  [[ -n "$sha_tool" ]] || {
    echo "benchmark-model self-test failed: sha256sum or shasum is required" >&2
    return 1
  }
  ln -s "$sha_tool" "$fake_bin/$(basename "$sha_tool")"
  local fixture_repo="$tmp/repo with space"
  mkdir -p "$fixture_repo/tools/dev" "$fixture_repo/tools/spike/models"
  cp "$repo_root/tools/dev/benchmark-model.sh" "$fixture_repo/tools/dev/benchmark-model.sh"
  chmod +x "$fixture_repo/tools/dev/benchmark-model.sh"
  printf 'fixture model\n' >"$fixture_repo/tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf"

  cat >"$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
if [[ "${1:-}" == "--version" ]]; then
  echo "cargo 1.97.0 (fixture)"
  exit 0
fi
printf '%s\n' "$*" >>"$COMPME_BENCH_FIXTURE_CARGO_LOG"
if [[ "$COMPME_BENCH_FIXTURE_MODE" == "build_fail" ]]; then
  echo "fixture build failure" >&2
  exit 42
fi
if [[ "$COMPME_BENCH_FIXTURE_MODE" == "missing_artifact" ]]; then
  echo '{"reason":"build-finished","success":true}'
  exit 0
fi
if [[ "$COMPME_BENCH_FIXTURE_MODE" == "malformed_cargo_json" ]]; then
  echo '{not-json'
  exit 0
fi
printf '{"reason":"compiler-artifact","target":{"name":"latency","test":true},"profile":{"opt_level":"0","debuginfo":2,"debug_assertions":true,"overflow_checks":true,"test":true},"features":[],"executable":"%s","fresh":false}\n' "$COMPME_BENCH_FIXTURE_TEST_EXE"
SH
  cat >"$fake_bin/rustc" <<'SH'
#!/usr/bin/env bash
echo "rustc 1.97.0 (fixture)"
SH
  cat >"$fake_bin/git" <<'SH'
#!/usr/bin/env bash
case "$*" in
  *"rev-parse HEAD") echo "e1c6a3e808730630029c033cb6af4b64740eae28" ;;
  *"status --porcelain --untracked-files=normal") : ;;
  *) exit 2 ;;
esac
SH
  cat >"$fake_bin/uname" <<'SH'
#!/usr/bin/env bash
echo Linux
SH
  cat >"$tmp/latency test" <<'SH'
#!/usr/bin/env bash
printf 'args=%s|latency=%s|model=%s|context_required=%s|gpu=%s|context=%s\n' \
  "$*" "${COMPME_REQUIRE_LATENCY_BUDGET:-}" "${COMPME_REQUIRE_MODEL_TESTS:-}" \
  "${COMPME_REQUIRE_MODEL_CONTEXT:-}" "${COMPME_MODEL_GPU_LAYERS:-}" \
  "${COMPME_MODEL_CONTEXT_TOKENS:-}" >>"$COMPME_BENCH_FIXTURE_TEST_LOG"
invocation="$(wc -l <"$COMPME_BENCH_FIXTURE_TEST_LOG" | tr -d '[:space:]')"
case "$COMPME_BENCH_FIXTURE_MODE" in
  pass_odd)
    case "$invocation" in
      1) elapsed=300 ;;
      2) elapsed=90 ;;
      3) elapsed=99 ;;
      *) exit 89 ;;
    esac
    echo "warm: ${elapsed}ms -> \"fixture\""
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.10s'
    ;;
  pass_even)
    case "$invocation" in
      1) elapsed=400 ;;
      2) elapsed=90 ;;
      3) elapsed=20 ;;
      4) elapsed=100 ;;
      *) exit 89 ;;
    esac
    echo "warm: ${elapsed}ms -> \"fixture\""
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.10s'
    ;;
  budget)
    echo 'warm: 500ms -> "fixture"'
    echo "thread 'warm_completion_under_500ms' panicked at fixture:"
    echo 'warm completion 500ms exceeded 500ms'
    echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.50s'
    exit 101
    ;;
  other_panic)
    echo 'warm: 90ms -> "fixture"'
    echo "thread 'warm_completion_under_500ms' panicked at fixture:"
    echo 'shutdown failed'
    echo 'test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.10s'
    exit 101
    ;;
  missing_sample)
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.10s'
    ;;
  zero_test)
    echo 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out; finished in 0.00s'
    ;;
  duplicate_sample)
    echo 'warm: 90ms -> "fixture"'
    echo 'warm: 91ms -> "fixture"'
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.10s'
    ;;
  vulkan)
    echo 'llama_model_load_from_file_impl: using device Vulkan0 (Fixture GPU) - 1 MiB free'
    echo 'load_tensors: offloaded 25/25 layers to GPU'
    echo 'warm: 97ms -> "fixture"'
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.10s'
    ;;
  vulkan_unobserved)
    echo 'warm: 97ms -> "fixture"'
    echo 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.10s'
    ;;
  *) exit 88 ;;
esac
SH
  chmod +x "$fake_bin/cargo" "$fake_bin/rustc" "$fake_bin/git" "$fake_bin/uname" "$tmp/latency test"

  local script="$fixture_repo/tools/dev/benchmark-model.sh"
  local cargo_log="$tmp/cargo.log"
  local test_log="$tmp/test.log"
  local case_name case_backend case_samples case_status
  run_case() {
    case_name="$1"
    case_backend="$2"
    case_samples="$3"
    case_status="$4"
    : >"$cargo_log"
    : >"$test_log"
    set +e
    PATH="$fake_bin" COMPME_BENCH_FIXTURE_MODE="$case_name" \
      COMPME_BENCH_FIXTURE_TEST_EXE="$tmp/latency test" \
      COMPME_BENCH_FIXTURE_CARGO_LOG="$cargo_log" \
      COMPME_BENCH_FIXTURE_TEST_LOG="$test_log" \
      "$script" --backend "$case_backend" --samples "$case_samples" \
      --output "$tmp/output space/out-$case_name" >"$tmp/$case_name.out" 2>"$tmp/$case_name.err"
    local got_status=$?
    set -e
    if [[ "$got_status" -ne "$case_status" ]]; then
      echo "benchmark-model self-test failed: $case_name exited $got_status, expected $case_status" >&2
      return 1
    fi
  }

  run_case pass_odd cpu 3 0
  [[ "$(wc -l <"$cargo_log" | tr -d '[:space:]')" == "1" ]]
  [[ "$(wc -l <"$test_log" | tr -d '[:space:]')" == "3" ]]
  grep -q '^test --locked -p model_client --test latency --no-run --message-format=json-render-diagnostics$' "$cargo_log"
  grep -q '^args=warm_completion_under_500ms --ignored --exact --nocapture --test-threads=1|latency=1|model=1|context_required=1|gpu=0|context=256$' "$test_log"
  ruby -rjson -e 'r=JSON.parse(File.read(ARGV[0])); abort unless r["status"]=="passed" && r["median_ms"]==99 && r["samples"].map{|s| s["elapsed_ms"]}==[300,90,99] && r["backend"]=={"requested"=>"cpu", "observed"=>"cpu", "cargo_feature"=>nil, "gpu_layers"=>0}' "$tmp/output space/out-pass_odd/report.json"

  run_case pass_even cpu 4 0
  ruby -rjson -e 'r=JSON.parse(File.read(ARGV[0])); abort unless r["status"]=="passed" && r["median_ms"]==95 && r["samples"].map{|s| s["elapsed_ms"]}==[400,90,20,100]' "$tmp/output space/out-pass_even/report.json"

  run_case budget cpu 2 1
  ruby -rjson -e 'r=JSON.parse(File.read(ARGV[0])); abort unless r["status"]=="budget_failed" && r["exit_status"]==1 && r["median_ms"]==500 && r["budget_failed_samples"]==2 && r["samples"].all?{|s| s["outcome"]=="budget_failed"}' "$tmp/output space/out-budget/report.json"

  run_case other_panic cpu 1 2
  ruby -rjson -e 'r=JSON.parse(File.read(ARGV[0])); abort unless r["status"]=="error" && r["median_ms"].nil? && r["samples"][0]["outcome"]=="error" && r["samples"][0]["elapsed_ms"]==90' "$tmp/output space/out-other_panic/report.json"
  run_case missing_sample cpu 1 2
  run_case zero_test cpu 1 2
  run_case duplicate_sample cpu 1 2
  run_case vulkan vulkan 1 0
  ruby -rjson -e 'r=JSON.parse(File.read(ARGV[0])); s=r["samples"][0]; abort unless r.dig("backend","observed")=="vulkan" && r.dig("backend","cargo_feature")=="vulkan" && s["offloaded_layers"]==25 && s["total_layers"]==25' "$tmp/output space/out-vulkan/report.json"
  grep -q -- '--features vulkan$' "$cargo_log"
  run_case vulkan_unobserved vulkan 1 2
  run_case missing_artifact cpu 1 2
  test ! -s "$test_log"
  ruby -rjson -e 'r=JSON.parse(File.read(ARGV[0])); abort unless r["status"]=="error" && r["samples"].empty? && r["build_artifact"].nil?' "$tmp/output space/out-missing_artifact/report.json"
  run_case malformed_cargo_json cpu 1 2
  test ! -s "$test_log"
  ruby -rjson -e 'r=JSON.parse(File.read(ARGV[0])); abort unless r["status"]=="error" && r["samples"].empty? && r["build_artifact"].nil?' "$tmp/output space/out-malformed_cargo_json/report.json"
  run_case build_fail cpu 1 2
  ruby -rjson -e 'r=JSON.parse(File.read(ARGV[0])); abort unless r["status"]=="error" && r["samples"].empty? && r["build_artifact"].nil?' "$tmp/output space/out-build_fail/report.json"

  if PATH="$fake_bin" "$script" --backend cpu --samples 1 \
    --output "$tmp/output space/out-pass_odd" >/dev/null 2>"$tmp/existing.err"; then
    echo "benchmark-model self-test failed: existing output directory was accepted" >&2
    return 1
  fi
  grep -q 'output path already exists' "$tmp/existing.err"
  if "$script" --self-test unexpected >/dev/null 2>"$tmp/argc.err"; then
    echo "benchmark-model self-test failed: extra self-test argument was accepted" >&2
    return 1
  fi
  grep -q '^usage: ' "$tmp/argc.err"
  "$script" --help >/dev/null
  if "$script" --backend cpu --samples 18446744073709551616 \
    --output "$tmp/huge" >/dev/null 2>"$tmp/huge.err"; then
    echo "benchmark-model self-test failed: overflowing sample count was accepted" >&2
    return 1
  fi
  grep -q '^usage: ' "$tmp/huge.err"

  rm -f "$fixture_repo/tools/spike/models/qwen2.5-0.5b-q4_k_m.gguf"
  : >"$cargo_log"
  : >"$test_log"
  set +e
  PATH="$fake_bin" COMPME_BENCH_FIXTURE_MODE=pass_odd \
    COMPME_BENCH_FIXTURE_TEST_EXE="$tmp/latency test" \
    COMPME_BENCH_FIXTURE_CARGO_LOG="$cargo_log" \
    COMPME_BENCH_FIXTURE_TEST_LOG="$test_log" \
    "$script" --backend cpu --samples 1 \
    --output "$tmp/output space/out-missing-model" \
    >"$tmp/missing-model.out" 2>"$tmp/missing-model.err"
  local missing_model_status=$?
  set -e
  if [[ "$missing_model_status" -ne 2 ]]; then
    echo "benchmark-model self-test failed: missing model exited $missing_model_status, expected 2" >&2
    return 1
  fi
  grep -q 'model missing or empty:' "$tmp/missing-model.err"
  test ! -s "$cargo_log"
  test ! -s "$test_log"
  test ! -e "$tmp/output space/out-missing-model"
  echo "benchmark-model self-test passed"
}

if [[ "${1:-}" == "--self-test" ]]; then
  [[ "$#" -eq 1 ]] || {
    usage
    exit 2
  }
  run_self_test
  exit 0
fi
if [[ "${1:-}" == "--help" ]]; then
  [[ "$#" -eq 1 ]] || {
    usage
    exit 2
  }
  echo "usage: $0 --backend cpu|vulkan --samples N --output DIR | --self-test"
  exit 0
fi

backend=""
samples=""
output_dir=""
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --backend)
      [[ "$#" -ge 2 ]] || {
        usage
        exit 2
      }
      backend="$2"
      shift 2
      ;;
    --samples)
      [[ "$#" -ge 2 ]] || {
        usage
        exit 2
      }
      samples="$2"
      shift 2
      ;;
    --output)
      [[ "$#" -ge 2 ]] || {
        usage
        exit 2
      }
      output_dir="$2"
      shift 2
      ;;
    *)
      usage
      exit 2
      ;;
  esac
done
[[ "$backend" == "cpu" || "$backend" == "vulkan" ]] || {
  usage
  exit 2
}
[[ "$samples" =~ ^[1-9][0-9]*$ ]] || {
  usage
  exit 2
}
[[ "${#samples}" -le 3 && "$samples" -le 100 ]] || {
  usage
  exit 2
}
[[ -n "$output_dir" ]] || {
  usage
  exit 2
}

model_path="$default_model"
run_benchmark
