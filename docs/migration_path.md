Access to frontier models like DeepSeek V4 and Mimo V2.5 completely shifts the bottleneck of a migration project. You are no longer fighting local hardware limits to squeeze a few functions into memory; you are fighting the model's "attention span" across massive context windows. Even with huge token limits, frontier models will hallucinate or gloss over subtle, undocumented C logic if you ask them to translate a 10,000-line monolith in one shot.

To achieve a battle-tested, industrial-grade v0.1 with robust documentation and maintainability, your automated pipeline needs to be broken into four distinct phases.

### Phase 1: The "Golden Master" Harness (Python)

Before touching a single line of Rust or translating any C, you must build the safety net. Python is the perfect glue for this kind of black-box testing.

* **The Fuzzing Agent:** Instruct your chosen model to write a comprehensive `pytest` suite. For a server like `thttpd`, this script should use the `requests` library and raw sockets to hammer the compiled C binary.
* **State Capture:** The script feeds the C binary a mix of valid static files, malformed headers, partial payloads, and abrupt disconnects. It records every exact HTTP response code, header order, and stdout log into a JSON file.
* **The Baseline:** This output is your "Golden Master." It represents the unquestionable truth of how the system *actually* behaves, undocumented quirks and all.

### Phase 2: Translation & Context Management

Even with advanced models, logical chunking is critical for accuracy.

* **Domain Grouping:** Break the C code down by functional domain (e.g., socket initialization, HTTP header parsing, file I/O) rather than just line counts.
* **Strict Prompting:** Feed a chunk to the model with rigid constraints: *"Translate this C to safe Rust. Maintain 1:1 structural behavior. Do not introduce modern async runtimes. Use standard library components and `mio` for event loops. Add exhaustive `rustdoc` to every struct and function."*
* **The Output:** You receive modularized Rust code that is heavily documented but structurally familiar to the original architecture, prioritizing stability over flashy syntax.

### Phase 3: The Differential Verification Loop

This is where the pipeline proves its worth for environments that demand zero downtime and absolute stability.

* **Compilation Checks:** The pipeline attempts to compile the new Rust workspace. If the borrow checker throws errors, the pipeline automatically feeds the exact compiler error and the problematic file back to the model for a localized fix.
* **Shadow Execution:** Once the codebase compiles, the pipeline spins up the Rust binary on a test port.
* **The Crucible:** The pipeline runs the exact same Python Golden Master test suite from Phase 1 against the new Rust binary.
* **Diffing the Results:** If the Rust binary drops a connection that the C binary handled gracefully, the test fails. The pipeline takes the failed Python test output, the expected baseline, and the associated Rust code, feeding it back to the AI: *"The legacy binary returned 400 Bad Request here, but the Rust binary dropped the socket. Fix the state handling."*

### Phase 4: The Modernization Pass

Once the Rust binary passes 100% of the Golden Master tests, the logic is verified. Now, you can leverage the models for enrichment without risking the core functionality.

* **Refactoring Run:** Send the verified, working Rust code back through the API.
* **The Polish:** Instruct the model: *"This code is functionally perfect. Do not alter the execution logic. Replace all legacy magic numbers with descriptive Enums. Ensure all public functions have complete `rustdoc` examples. Standardize error handling using the `thiserror` crate."*

This pipeline guarantees that the resulting application is demonstrably identical in function to the legacy system, but wrapped in Rust's memory safety and modern documentation standards.

Are you planning to build the orchestration logic for this pipeline as a set of standalone bash/Python scripts, or are you looking to integrate it directly into a CI/CD workflow like GitHub Actions from the start?