# Linux model latency evidence — 2026-09-19

This record covers roadmap item 30 on one Linux machine. It does not establish
a cross-platform baseline: Windows, macOS and CUDA still require their own
target runs. The 500 ms warm 12-token completion budget was not changed.

## Host and test contract

- Intel Core i9-9880H: 8 physical cores, 16 logical CPUs, maximum 4.8 GHz.
- NVIDIA Quadro RTX 4000, 8 GiB.
- Rust 1.97.0 (`x86_64-unknown-linux-gnu`).
- Qwen2.5 0.5B Q4_K_M, 397,807,520 bytes.
- `COMPME_MODEL_CONTEXT_TOKENS=256`; CPU uses
  `COMPME_MODEL_GPU_LAYERS=0`.
- `warm_completion_under_500ms` calls the production `warm_up()`, then times
  `complete(terse_continuation_prompt("The quick brown fox"), 12)`.
- Each sample below is a new test process. Model load and `warm_up()` are
  outside the reported interval. The GGUF was in the filesystem page cache.

The canonical CPU command that builds and runs the same test/profile is:

```sh
nix-shell -p cmake pkg-config clang libclang --run '
  export PATH=/home/mudrii/.rustup/toolchains/1.97.0-x86_64-unknown-linux-gnu/bin:$PATH
  export LIBCLANG_PATH="$(nix-build --no-out-link "<nixpkgs>" -A llvmPackages.libclang.lib)/lib"
  export LD_LIBRARY_PATH="$(nix-build --no-out-link "<nixpkgs>" -A stdenv.cc.cc.lib)/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  COMPME_REQUIRE_LATENCY_BUDGET=1 \
  COMPME_REQUIRE_MODEL_TESTS=1 \
  COMPME_REQUIRE_MODEL_CONTEXT=1 \
  COMPME_MODEL_GPU_LAYERS=0 \
  COMPME_MODEL_CONTEXT_TOKENS=256 \
  cargo test --locked -p model_client --test latency \
    warm_completion_under_500ms -- \
    --ignored --exact --nocapture --test-threads=1
'
```

For the five-sample distribution, the already-built test executable was
invoked directly in five fresh processes, with the same `COMPME_*` values and
test arguments. This avoided Cargo/build activity during the measurement; the
timer remains inside the test around `complete()` in either invocation.

## Results

The current portable CPU build produced **1,752, 1,754, 1,777, 1,821 and
1,796 ms**. Median was **1,777 ms**; all five samples failed the unchanged
500 ms assertion. The generated text was identical in every run.

The Rust test harness used the unoptimized `test` profile. Its linked
llama.cpp C/C++ library was independently confirmed as CMake `Release` with
`-O3 -DNDEBUG`. The portable cache had `GGML_NATIVE`, `GGML_AVX`,
`GGML_AVX2`, `GGML_FMA` and `GGML_OPENMP` all off.

Temporary diagnostic overrides varied only context threads and requested
tokens. They were tagged `[DEBUG-item30]`, were never committed, and were
removed after the measurements.

| Probe | Samples (ms) | Finding |
| --- | --- | --- |
| Portable, 4 threads, 12 tokens | 1,903 / 1,875 / 1,836 | Interleaved control remained well over budget |
| Portable, 8 threads, 12 tokens | 1,091 / 1,084 / 1,109 | Material improvement, still more than twice the budget |
| Portable, 4 threads, 1 / 2 / 4 / 8 / 12 tokens | 1,131 / 1,219 / 1,296 / 1,632 / 1,941 | One token already exceeds the budget; fixed work through the first generation step dominates later-token cost in this case |
| `target-cpu=native`, test profile, 4 threads, 1 / 12 tokens | 1,080 / 1,828 | Native SIMD alone did not resolve the failure |
| `target-cpu=native`, test profile, 8 threads, 1 / 12 tokens | 617 / 1,118 and 1,131 | Thread count plus native SIMD remained red |
| `target-cpu=native`, release profile, 8 threads, 1 / 12 tokens | 656 / 1,193 and 1,187 | Optimizing the Rust harness did not improve this bounded sample |

The complete thread sweep was topology-sensitive: 1, 2, 4, 8 and 16 threads
measured 7,066, 3,597, 2,013, 1,285 and 4,205 ms respectively. The 16-thread
regression makes a portable default inferred from this one CPU unsafe. The
follow-up interleaved 4/8-thread samples above confirm the local benefit
without supporting a cross-platform policy.

The native-SIMD diagnostic used a separate target directory:

```sh
CARGO_TARGET_DIR=/tmp/compme-native-target \
CARGO_BUILD_JOBS=4 \
RUSTFLAGS='-C target-cpu=native' \
cargo test --locked -p model_client --test latency --no-run
```

Its CMake cache confirmed `CMAKE_BUILD_TYPE=Release` and `GGML_NATIVE=ON`.
The release-profile comparison changed the command to `cargo test --release`.
Neither configuration is suitable as an unconditional portable build: the
binary would assume the build machine's instruction set.

## Vulkan boundary

The earlier Quadro RTX 4000 diagnostic offloaded layers to `Vulkan0` and
measured **2,075 ms**, also failing 500 ms. It used the same prompt, token
count and context size. That run was diagnostic because other builds were
active. Its `/tmp/compme-vulkan-target` artifact no longer exists, so Vulkan
was not represented as a fresh calibrated result in the initial September 19 record.

The following reproducible command is reconstructed from the original
separate Vulkan build and runtime invocations:

```sh
nix-shell -p cmake pkg-config clang libclang \
  vulkan-headers vulkan-loader shaderc --run '
  export PATH=/home/mudrii/.rustup/toolchains/1.97.0-x86_64-unknown-linux-gnu/bin:$PATH
  export LIBCLANG_PATH="$(nix-build --no-out-link "<nixpkgs>" -A llvmPackages.libclang.lib)/lib"
  export LD_LIBRARY_PATH="$(nix-build --no-out-link "<nixpkgs>" -A vulkan-loader)/lib:/run/opengl-driver/lib:$(nix-build --no-out-link "<nixpkgs>" -A stdenv.cc.cc.lib)/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  export VK_DRIVER_FILES=/run/opengl-driver/share/vulkan/icd.d/nvidia_icd.json
  CARGO_TARGET_DIR=/tmp/compme-vulkan-target \
  CARGO_BUILD_JOBS=2 \
  COMPME_REQUIRE_LATENCY_BUDGET=1 \
  COMPME_REQUIRE_MODEL_TESTS=1 \
  COMPME_REQUIRE_MODEL_CONTEXT=1 \
  COMPME_MODEL_GPU_LAYERS=999 \
  COMPME_MODEL_CONTEXT_TOKENS=256 \
  cargo test --locked -p model_client --features vulkan --test latency \
    warm_completion_under_500ms -- \
    --ignored --exact --nocapture --test-threads=1
'
```

### Fresh Vulkan calibration — 2026-09-20

The documented Vulkan setup was rebuilt from an absent target directory with
`CARGO_TARGET_DIR=/tmp/compme-vulkan-target`, `CARGO_BUILD_JOBS=2`, the
`model_client/vulkan` feature, the pinned Nix Vulkan loader and NVIDIA ICD.
The CMake cache recorded `CMAKE_BUILD_TYPE=Release`, `GGML_VULKAN=ON`,
`GGML_CUDA=OFF` and `GGML_NATIVE=OFF`. Runtime kept the strict test contract:
`COMPME_REQUIRE_LATENCY_BUDGET=1`, `COMPME_REQUIRE_MODEL_TESTS=1`,
`COMPME_REQUIRE_MODEL_CONTEXT=1`, `COMPME_MODEL_GPU_LAYERS=999` and
`COMPME_MODEL_CONTEXT_TOKENS=256`.

Three invocations of the already-built test executable in fresh processes
measured **101, 97 and 72 ms**, for a **97 ms median**. All three passed the
unchanged 500 ms budget and produced the same completion. llama.cpp identified
`Vulkan0` as the NVIDIA Quadro RTX 4000, assigned all 24 KV-cache layers to it,
and reported **25/25 model layers offloaded**, a 373.71 MiB Vulkan model buffer
and a 149.25 MiB Vulkan compute buffer. Concurrent `nvidia-smi` sampling also
observed each latency-test process on GPU 0. The desktop compositor and
Sunshine were active GPU clients during the samples, so this is a calibrated
interactive-desktop result rather than an exclusive-GPU benchmark.

A follow-up paired comparison used that exact Vulkan-enabled test
executable in six more fresh processes, interleaving CPU then GPU three times.
The only changed variable was `COMPME_MODEL_GPU_LAYERS` (`0` versus `999`);
context 256, the prompt, the 12-token output budget and the strict 500 ms
assertion stayed fixed.

| Pair | CPU, 0/25 layers offloaded | GPU, 25/25 layers offloaded |
|---|---:|---:|
| 1 | 1,833 ms (failed budget) | 120 ms (passed) |
| 2 | 1,806 ms (failed budget) | 110 ms (passed) |
| 3 | 1,865 ms (failed budget) | 122 ms (passed) |
| Median | **1,833 ms** | **120 ms** |

All six produced the exact same completion. This same-build comparison shows a
15.3x median improvement from full Vulkan offload on this machine and confirms
that the CPU path still fails the default contract. It supports the existing
Vulkan opt-in for this tested GPU; one card still cannot establish a portable
product default.

The same Vulkan executable's complete six-test ignored real-model suite passed
in 16.52 seconds. That includes the strict under-250-ms cancellation test,
prefix-cache equivalence, multi-candidate generation and grammar output
vetting. Its diagnostic quality probe fixed 8/9 typo cases with 0/4 false
fixes; an additional strict warm completion measured 69 ms.

Separately, the dedicated `tests/quality.rs` 21-case corpus gate ran through
the same Vulkan build and strict model/context environment. It passed 20/21
cases (**95%**) against the unchanged 80% threshold (17 cases required), with
the pinned `typo-occured` miss as its only failed case. The harness passed in
14.01 seconds and again reported 25/25 layers offloaded to the Quadro RTX 4000.

Vulkan remains an explicit build-time feature, and full offload in this run was
selected explicitly through `COMPME_MODEL_GPU_LAYERS=999`. This result does not
change the portable CPU default, which still fails the budget on this host, and
does not justify making a GPU backend a product default from one card. No code,
budget, model, prompt or production setting changed. The earlier 2,075 ms run
remains useful historical evidence of a contaminated diagnostic, but the large
difference is not a controlled speedup comparison because that run overlapped
other builds and its artifact was unavailable for an otherwise identical
rerun. The fresh result establishes only that the existing optional Vulkan
backend meets the contract on this Quadro RTX 4000 under the conditions above.

## Follow-up: decode phase attribution

The follow-up review reran the unchanged strict test on this host: **1,844 ms**,
again failing the 500 ms assertion. Temporary timing probes in
`complete_on_worker` then measured the prompt `context.decode` call and the
sum of generated-token `context.decode` calls separately from the whole worker
call. They printed only timings and token counts, not prompt content. Model,
context size, thread defaults, requested tokens and the test assertion were
unchanged. Three fresh test processes produced:

| Sample | Whole worker (ms) | Prompt decode (ms) | Generated-token decodes (ms) | Generated tokens |
|---|---:|---:|---:|---:|
| 1 | 1,796.293 | 945.406 | 845.505 | 12 |
| 2 | 1,830.608 | 978.916 | 846.337 | 12 |
| 3 | 1,807.944 | 959.866 | 842.739 | 12 |

The existing end-to-end timer reported 1,796 / 1,830 / 1,808 ms; all failed.
Compilation finished before each measurement, and no concurrent Rust builds
were running. These are instrumented diagnostic samples, not a new backend or
cross-platform baseline. The timing probes were removed afterward and
`git diff -- crates/model_client/src/lib.rs` was empty.

Prompt decoding accounts for about 53% and generated-token decoding about 47%
of the measured worker time. The remaining work, including tokenization,
sampling, text conversion and cache bookkeeping, was under 6 ms in each sample.
This rejects sampling/text-conversion overhead as the dominant cause on this
host. Prompt decoding alone exceeds the complete-request budget, so eliminating
only generated-token work cannot bring this workload below 500 ms. Further
investigation should measure the prompt-decode backend/model path and calibrated
GPU execution; these results do not justify relaxing the budget or changing
portable thread/SIMD defaults.

## Repeatable runner validation — 2026-09-20

`tools/dev/benchmark-model.sh` now automates the existing strict measurement.
See [Development](DEVELOPMENT.md#repeatable-linux-model-benchmarks) for its
CLI, prerequisites, reports, and exit statuses. Add Ruby to the Nix shells
above when using the runner. Each invocation performs one Cargo build command
and selects its test executable from Cargo JSON; every sample is a fresh
process, with loading and warm-up outside the measured interval.

The implementation was validated on the same i9-9880H / Quadro RTX 4000 host,
Linux 6.18.52, Rust 1.97.0, using the working tree based on `e1c6a3e`.
Reports correctly recorded `git.dirty=true`: the new runner and its integration
were not yet committed. Production model code and the existing latency test
were unchanged. Both builds reused their current Cargo artifacts. The model
SHA-256 was `ca6f8885c1d6a14025e705295fe1b240ad5a30c4c696215a341d7e6610a26484`.

| Backend | Three samples (ms) | Median (ms) | Runner result |
|---|---|---|---|
| CPU, GPU layers 0 | 1,736 / 1,765 / 1,769 | 1,765 | Exit 1; all samples retained, all above budget |
| Vulkan, requested GPU layers 999 | 94 / 75 / 74 | 75 | Exit 0; all below budget, native Vulkan diagnostics prove 25/25 layers offloaded for every sample |

CPU and Vulkan were invoked sequentially, with no concurrent build/inference
workloads during sampling, using their respective CPU and Vulkan artifacts.
These runs validate the runner's success and budget-failure paths; they do
not establish an optimization, a same-binary paired comparison, or performance
on other hardware. The fixed context remained 256 tokens, output request
12 tokens, and acceptance remained strictly less than 500 ms for every sample.
The hermetic CLI self-tests additionally cover missing/duplicate measurements,
zero-test success, unrelated panics, missing models, build failure, output
preservation, and unproven Vulkan offload.

## Conclusion

The Linux CPU failure is reproducible and is not explained by the earlier
concurrent compilation. On this host, the fixed path through prompt decode,
first-token sampling and its decode already takes more than 500 ms at a
one-token output budget; this probe does not attribute that time to one phase.
More threads and native SIMD improve the result but do not satisfy the
contract; Rust release mode made no improvement in the bounded comparison. No
production code change is justified by this single machine: a thread default
is topology-sensitive, and `target-cpu=native` would break portable artifacts.
Linux Vulkan is now calibrated and green on the Quadro RTX 4000. Item 30
remains open for Windows, macOS and CUDA target measurements and for any
portable-CPU backend/model strategy that preserves the existing latency
budget.
