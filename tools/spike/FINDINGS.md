# A0 Spike Findings

| Probe | Result | Notes |
|-------|--------|-------|
| P1 build | PASS | M4 Max, macOS 25.5, rustc 1.95.0, cmake 4.3.3, Apple clang 21. First `cargo build --release` (incl. cmake+llama.cpp compile) = 43s. No toolchain fixes, no extra flags, no API adjustments needed — task code compiled verbatim against llama-cpp-2 0.1.146. `metal` feature works; runtime offloaded 25/25 layers to GPU (`with_n_gpu_layers(999)`), device = MTL0 (Apple M4 Max), MTLGPUFamilyApple9/Metal4. `n_ctx_train=32768`. First run 8s (embedded Metal shader lib load ~6.9s dominates; cached after). Benign info logs: "tensor API disabled for pre-M5/pre-A19" (M4 uses std simdgroup matmul), and token_embd "CPU_REPACK -> using CPU instead". |
| P2 infer | | |
| P3 ax read | | |
| P4 caret | | |
| P5 tap | | |
| P6 overlay | | |
| P7 smoke | | |
