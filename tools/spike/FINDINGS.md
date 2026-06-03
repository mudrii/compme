# A0 Spike Findings

| Probe | Result | Notes |
|-------|--------|-------|
| P1 build | PASS | M4 Max, macOS 25.5, rustc 1.95.0, cmake 4.3.3, Apple clang 21. First `cargo build --release` (incl. cmake+llama.cpp compile) = 43s. No toolchain fixes, no extra flags, no API adjustments needed — task code compiled verbatim against llama-cpp-2 0.1.146. `metal` feature works; runtime offloaded 25/25 layers to GPU (`with_n_gpu_layers(999)`), device = MTL0 (Apple M4 Max), MTLGPUFamilyApple9/Metal4. `n_ctx_train=32768`. First run 8s (embedded Metal shader lib load ~6.9s dominates; cached after). Benign info logs: "tensor API disabled for pre-M5/pre-A19" (M4 uses std simdgroup matmul), and token_embd "CPU_REPACK -> using CPU instead". |
| P2 infer | PASS | Real `LlamaCompleter` (owns `LlamaBackend`+`LlamaModel`) implements `spike::completion::Completer`; greedy decode of ≤8 tokens, fresh `LlamaContext` per `complete()`. Headless acceptance test (`tests/model_integration.rs`): warm 12-token completion = **35ms** (assert `<500ms` PASS — ~14x margin). `p2_infer` runs the full `suggest("Dear team, I wanted to ", 23, &completer, 4)` pipeline end-to-end → capped 4-word output `"ask about the \"C\""`; steady-state warm `suggest()` latency **38ms** (first cold run 112ms incl. page-in). Latency budget is comfortably met — no need to re-tier the model. **base-vs-instruct:** model is the **instruct** variant; greedy continuation of a bare prefix drifts toward instruct chatter — note the trailing `"C` (heading into a quoted/clarifying fragment, e.g. `"CSV"`/a question) rather than a clean sentence completion. For A1 this argues for either (a) a base/text-completion GGUF for inline FIM-style continuation, or (b) keeping instruct but prompting/stop-token shaping + the `cap_words` guard (already trims the chatter to ≤4 words here). API: llama-cpp-2 0.1.146 used verbatim, no adjustments (`token_to_str`/`Special::Tokenize` emit deprecation warnings only). |
| P3 ax read | | |
| P4 caret | | |
| P5 tap | | |
| P6 overlay | | |
| P7 smoke | | |
