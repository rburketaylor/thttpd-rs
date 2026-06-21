# Running the Security Tools Locally

> One-page howto for Miri, AddressSanitizer, and `cargo-fuzz` against the HTTP
> parser crates. These are the same jobs that run in CI (`.github/workflows/`
> `miri.yml`, `sanitizers.yml`, `fuzz.yml`) — this page is for running them on
> your own machine, typically when chasing a failure or doing a longer fuzz run.

## Prerequisite: the nightly toolchain

The workspace pins stable 1.85 via `rust-toolchain.toml` — **do not change
that**. Miri, `-Z sanitizer=address`, and `cargo-fuzz` all require nightly, so
install it separately and prefix commands with `cargo +nightly`:

```bash
rustup toolchain install nightly --component miri --component rust-src
cargo +nightly miri setup   # one-time Miri sysroot build
```

All commands below run from the `rust/` directory.

## Miri (undefined-behavior detector)

Miri interprets the MIR and flags UB: out-of-bounds accesses, data races,
invalid pointer use, unaligned access, memory leaks under `Debug`, etc.

```bash
cd rust
MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-permissive-provenance" \
  cargo +nightly miri test -p thttpd-http --lib
MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-permissive-provenance" \
  cargo +nightly miri test -p thttpd-tdate --lib
```

- `-Zmiri-disable-isolation` lets env/time-dependent tests run.
- `-Zmiri-permissive-provenance` is needed because the parser does raw byte
  slicing. **Deprecation note:** on newer nightlies this flag is renamed to
  `-Zmiri-provenance-gc`; if the pinned nightly rejects the old name, swap to
  the new one — or drop it entirely if parser tests pass without it.

**Interpreting output:** Miri prints `error: Undefined Behavior` with a stack
trace when it finds UB. A clean run ends with `test result: ok`.

## AddressSanitizer (ASan)

ASan instruments compiled code to catch heap/stack buffer overflows,
use-after-free, and double-free at runtime.

```bash
cd rust
RUSTFLAGS="-Z sanitizer=address" \
  cargo +nightly test -p thttpd-http --lib --target x86_64-unknown-linux-gnu
```

The explicit `--target` is required because sanitizer instrumentation only
applies when cross-compiling to a listed target triple (even on the host).

**Interpreting output:** ASan prints `==PID==ERROR: AddressSanitizer: ...` with
the faulting access and a stack trace. A clean run ends with
`test result: ok`.

> This plan currently gates ASan only. Do **not** claim UBSan unless a separate,
> verified `-Z sanitizer=...` command is added.

## cargo-fuzz (libFuzzer)

`cargo-fuzz` generates malformed inputs and looks for panics/crashes in the
parser. The harness lives in `rust/fuzz/` (a standalone package, not a workspace
member, because `libfuzzer-sys` needs nightly).

### CI smoke run (60s, bounded)

```bash
cd rust
cargo +nightly fuzz run parse_request -- -max_total_time=60 -runs=0
cargo +nightly fuzz run parse_url    -- -max_total_time=60 -runs=0
```

### Longer local run

```bash
cd rust
cargo +nightly fuzz run parse_request -- -max_total_time=600   # 10 min
```

Inputs that crash the target are written to `rust/fuzz/artifacts/` as
`crash-*` / `leak-*` files. Replay a specific crash:

```bash
cargo +nightly fuzz run parse_request -- rust/fuzz/artifacts/crash-<sha>
```

### Corpus

`rust/fuzz/corpus/parse_request/` and `.../parse_url/` hold seed inputs if
available. The differential harness (`harness/golden/`) currently holds only
metadata, not raw request bytes, so the corpus may start empty — libFuzzer
generates inputs from scratch in that case. Drop any captured raw requests
into the corpus directory to seed future runs.

## What each tool catches (and doesn't)

| Tool | Catches | Does not catch |
|------|---------|----------------|
| Miri | UB in the executed code path: OOB access, invalid pointers, data races, leaks (under `Debug`) | Logic bugs, code Miri doesn't interpret (FFI) |
| ASan | Heap/stack OOB writes & reads, use-after-free, double-free, stack-use-after-return | Logic bugs; leaks (use LeakSanitizer separately) |
| cargo-fuzz | Panics and crashes reachable from the fuzz target entry points | UB on un-fuzzed code paths; logic bugs that don't panic |

Together with the `unsafe`-budget gate (`pipeline/audit_unsafe.sh`) and the
supply-chain jobs (`make security`), these are the runtime half of the security
claims in [`MIGRATION_REPORT.md`](MIGRATION_REPORT.md).
