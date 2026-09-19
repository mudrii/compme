# Authorized implementation evidence — 2026-09-19

Scope: review-list items 1–6 and 19–35, on the working tree based on
`6c515f1`. This is an implementation record, not a release or native acceptance
claim. Pending-work status remains in [ROADMAP](ROADMAP.md).

Sol high agents implement the platform and memory slices. Astra reviews the
changes independently. Its first review caught a Linux result-publication
race and disabled-memory hydration; both findings must be resolved before
the associated work is described as verified.

## External prerequisites

- **19:** Qfd §22 requires a recorded physical-key baseline on the old Mac
  build before replacing Carbon registration. No granted Mac session has
  been identified here. The baseline and post-change acceptance remain open.
- **27–28:** no GNOME-Wayland, KDE-Wayland or sway session is available for
  the comparative IME/portal spike. Selecting a compositor strategy without
  that evidence would skip the plan's decision gate.
- **30:** Linux CPU and hardware Vulkan diagnostics were collected here;
  Windows, macOS and CUDA measurements remain open. Calibrated distributions
  of latency, rather than individual diagnostic samples, are still required.
- **31:** the Windows adapter's event/input/overlay/shell implementation
  (review items 7–18) is outside this request. Packaging its current
  fail-closed binary as a usable Windows release would be misleading.
- **32–33:** the AppImage assembler is experimental. Ubuntu 24.04 assembly
  and extracted startup now pass; real X11 product acceptance and a declared
  distribution floor precede release integration. No cross-platform artifact
  is published by this change. The existing macOS release flow is retained.

## Optional GPU build

`app` and `model_client` forward opt-in `vulkan` and `cuda` features to the
exact-pinned vendored llama dependency. Default feature graphs remain CPU-only
off macOS; Metal remains the macOS default. The feature checker verifies both
opt-in off-mac target graphs without enabling dynamic backends.

A Linux `cargo build --locked -p app --features vulkan` with Nix-provided
Vulkan headers, loader and shaderc completed successfully. `gpu.yml` adds a
scheduled Linux Vulkan SDK build. CUDA and Windows SDK builds are unverified;
neither runtime support nor latency is inferred from dependency resolution.

The local NVIDIA Quadro RTX 4000 subsequently executed the Vulkan model test
with layers offloaded to `Vulkan0`. Warm completion measured **2,075 ms** and
failed the same 500 ms limit. The first runtime attempt failed to locate
`libvulkan.so.1`; adding the Nix Vulkan loader's library directory resolved
that environment prerequisite. The successful backend execution does not
make the failing latency gate green. CUDA and other operating systems remain
unmeasured.

A real Linux CPU run on the Intel Core i9-9880H with the pinned Qwen2.5 0.5B
Q4_K_M model, 256-token context and zero GPU layers produced a 12-token warm
completion in **1,778 ms**. The existing 500 ms test failed as designed:
`warm completion 1778ms exceeded 500ms`. Other implementation builds were
active, so this is diagnostic evidence, not a calibrated cross-platform
baseline. No threshold was relaxed. Reproduce with the three
`COMPME_REQUIRE_LATENCY_BUDGET`, `COMPME_REQUIRE_MODEL_TESTS` and
`COMPME_REQUIRE_MODEL_CONTEXT` variables set to `1`,
`COMPME_MODEL_GPU_LAYERS=0`, `COMPME_MODEL_CONTEXT_TOKENS=256`, then
`cargo test --locked -p model_client --test latency warm_completion_under_500ms
-- --ignored --exact --nocapture --test-threads=1`.

## Vendored extension review

The published `llama-cpp-2` **0.1.156** archive was inspected from
the crates.io static archive endpoint (`llama-cpp-2-0.1.156.crate`).
Archive SHA-256:
`b6450f6d59b4d758b0d92f50a640136c713ec7e08d8ebb0595db03c939b502d4`.
Its `src` tree contains no `abort_flag`, `abort_callback`, or `abort_poll`
API. The context owner does not provide an equivalent lifetime-owning
cancellation facility. Consequently item 35 remains blocked on an equivalent
upstream API: all three exact version pins, both workspace patches and the
vendored implementation are retained. No unverified dependency upgrade was
made.

## Validation record

The final integrated portable run passed **1,856 tests**, with **49 ignored**,
and portable-workspace clippy with warnings denied. After all review repairs,
the Linux native harness passed **40/40** tests in Xvfb/AT-SPI, including the
new geometry, XTEST and shortcut tests. The owner also reran the XTEST test
with `us,ru` keyboard groups to prove non-primary-group refusal. Astra's final
review reports no remaining actionable finding in the implemented slices.

Windows MSVC all-target clippy passed for `platform_windows` and
`platform_linux`, with warnings denied. macOS aarch64 all-target clippy passed
for `platform_macos` and `platform_linux`. Portable-workspace rustdoc with
warnings denied passed after correcting a Windows-only helper's conditional
intra-doc link. Memory's 59 tests and the AppImage assembler
self-test were also run independently by Astra.

The portable CPU release binary built successfully inside an Ubuntu 24.04
container (local image ID
`44d80ac8c1697d9020cd3a0fad93f53ef195d9be50ba98c4ef1aa4f477978655`).
The final reviewed source was rebuilt, and an AppImage was assembled using supplied, digest-verified linuxdeploy,
appimagetool and type-2 runtime binaries. The extracted `AppRun` passed the
missing-model startup smoke in a fresh container without a display or
accessibility bus. That run exposed an overly narrow smoke assertion:
`Blocked(AccessibilityUnavailable)` legitimately outranks missing-model status.
The helper now accepts that status while still requiring all model-recovery
logs, clean exit and absence of completion requests; its regression passes.
The local experimental artifact is
`/tmp/compme-portable-target/compme-0.1.6-linux-x86_64-reviewed.AppImage`.
It is not a published or desktop-qualified release.

Native GUI results belong in `docs/ACCEPTANCE.md`; no macOS live gate is
advanced by portable tests or cross-target compilation. The workspace test
count anchors were restamped from 2,190 to 2,222 using the macOS-visible
test deltas (+11 Windows helpers, +2 Linux/engine, +3 shortcuts, +16 memory).
The actual emitted count still must be verified by the macOS policy gate.

`tools/dev/check.sh` was run in its documented order. Formatting passed;
command **2/56**, workspace clippy, failed on this Linux host with
`objc2 only works on Apple platforms` and Apple-framework link-kind errors.
The wrapper stopped there, so subsequent commands were not run by that full
gate. Independently run portable/native checks are listed above rather than
being represented as a green Full Local Gate. No commit or release was made.
The release-policy checker's self-test passed; its full run likewise reaches
the macOS-only workspace test enumeration and cannot finish here.
