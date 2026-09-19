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
was not represented as a fresh calibrated result in this record.

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

## Conclusion

The Linux CPU failure is reproducible and is not explained by the earlier
concurrent compilation. On this host, the fixed path through prompt decode,
first-token sampling and its decode already takes more than 500 ms at a
one-token output budget; this probe does not attribute that time to one phase.
More threads and native SIMD improve the result but do not satisfy the
contract; Rust release mode made no improvement in the bounded comparison. No
production code change is justified by this single machine: a thread default
is topology-sensitive, and `target-cpu=native` would break portable artifacts.
Item 30 remains open for calibrated Vulkan and other target measurements or a
backend/model strategy that preserves the existing latency budget.
