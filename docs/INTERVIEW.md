# Interview Presenter Guide

Use this as the main screen-share path. The goal is to tell one coherent story
without reading the whole repository aloud: this is a behavior-preserving
migration from C to Rust, backed by executable evidence.

## Quick Stats

- **Project**: C → Rust port with differential testing
- **Tests**: 535 total (105 differential, 256 unit, 63 comparator, 31 proxy)
- **Status**: Behavioral parity on 105 scenarios; 3 known deviations tracked
- **Demo command**: `make demo`
- **Full verification**: `make verify`

## Navigation

- **Main track:** thesis, repo shape, failure story, proof, migration safety, AI discipline.
- **Live command:** `make demo`.
- **Complete gate to mention:** `make verify`.
- **If interrupted:** come back to this line: "The method is the product: the C binary is the oracle, differential tests make behavior visible, and the proxy makes cutover reversible."

## 0:00-0:45 — Thesis

**Say:**

> This is not a new web server. It is a modernization case study: port
> `sthttpd 2.27.0` from C to Rust without losing the behavior that production
> users depend on.

The key idea is that legacy documentation is not the specification. Observable
production behavior is. This repo keeps the C binary in-tree as the executable
reference and tests the Rust port against it.

**Show:**
- [`README.md`](../README.md) for the project summary and test inventory.
- `legacy/` as the retained C reference implementation.

**Transition:**
> The repo is organized around making that claim testable.

## 0:45-1:45 — Shape

**Say:**

> The structure mirrors the migration method: reference, port, oracle, and
> evidence.

**Show these directories:**
- `legacy/` — the C reference, retained deliberately.
- `rust/` — 8 Rust crates that mirror the original C modules.
- `harness/` — the differential engine and request scenarios.
- `knowledge/` — C-to-Rust migration records.
- `docs/` — the operational story.

**Key architectural decision:**
> The server stays single-threaded with `mio` so its shape remains close to the
> C event loop. The migration proxy uses `tokio` and `hyper` because proxying
> many backend requests is a different workload.

**Evidence:** [`docs/ADR-0002-async-runtime-split.md`](ADR-0002-async-runtime-split.md)

**Transition:**
> That structure was not obvious at the start; it came from an early failure.

## 1:45-2:45 — Failure Story

**Say:**

> The first pass reported every implementation phase complete. Files existed,
> it compiled, tests were collected, and the server did not answer requests.

The lesson: structural completion is not behavioral completion. After that,
every gate was rewritten as observable behavior: a real request returns a real
response, the C binary serves the same request, and the harness compares both
outputs.

**Bug categories:**
- Missing response headers
- Missing HTTP features (`If-Modified-Since`, `Range`, `HEAD`)
- Security gaps (symlink escape, directory traversal)
- CGI output handling
- Nondeterminism that needed explicit normalization

**Memorable edge cases:**
- CGI stdin deadlock
- Negative `Content-Length` wrapping to enormous body size
- Parser state resetting on every read

**Evidence:** [`JOURNEY.md`](../JOURNEY.md)

**Transition:**
> Once the behavior harness existed, adding coverage became cheap.

## 2:45-4:15 — Proof

**Run:**
```bash
make demo
```

**Say:**

> `make demo` inventories the verification layers and runs representative
> comparisons. `make verify` is the full gate.

The credibility point:
> Normalization is explicit and narrow. Timestamps, temp paths, dynamic ports,
> and CGI `PWD` can differ between processes, but normalized bodies are still
> hashed and compared. The oracle has its own tests so passing scenarios mean
> what they appear to mean.

**Numbers:**
- 105 C-vs-Rust differential scenarios
- 256 Rust unit tests
- 63 comparator unit tests
- 31 proxy integration tests
- 535 automated tests total

**Evidence:**
- [`harness/diff_engine.py`](../harness/diff_engine.py)
- [`harness/test_diff_engine.py`](../harness/test_diff_engine.py)
- [`harness/tests/test_differential.py`](../harness/tests/test_differential.py)

**Transition:**
> Behavioral parity is only phase one. The next question is how to cut over
> safely.

## 4:15-5:45 — Migration Safety

**Say:**

> `thttpd-migrate` is the cutover machinery. It sits in front of C and Rust and
> lets traffic move gradually without changing either server.

**Key points:**
- **Shadow mode:** C serves users, Rust receives mirrored requests, responses are diffed, users are unaffected.
- **Canary routing:** weighted C-to-Rust shifts (1%, 10%, 50%, 100%).
- **Health and circuit breaker:** unhealthy Rust is removed from routing.
- **Metrics and request IDs:** production investigation has handles.
- **Rollback:** one control-socket command moves new traffic back to C while in-flight requests finish normally.

**Be explicit about limits:**
> The repo does not hide gaps. Known operational deviations are tracked with
> legacy behavior, current Rust behavior, impact, and disposition.

**Evidence:**
- [`docs/MIGRATION.md`](MIGRATION.md)
- [`docs/RISKS.md`](RISKS.md)

**Transition:**
> That same discipline is how AI assistance was kept useful instead of merely productive.

## 5:45-6:30 — AI Assistance

**Say:**

> AI helped build the project, but the interesting part is the control system
> around it. The first generated pass looked complete and served nothing. The
> later process made every claim observable, used the C binary as the oracle,
> and tested the comparison engine itself.

**Closing line:**
> The value is not in generation. It is in verification.

**Evidence:** [`docs/AI_ASSISTANCE.md`](AI_ASSISTANCE.md)

## Follow-Up Questions

Use these when the interviewer asks. Do not open them during the main track unless time allows.

| Question | Go to |
|---|---|
| How do you know the port matches C behavior? | [`JOURNEY.md`](../JOURNEY.md) and `harness/tests/test_differential.py` |
| What does normalization hide? | README "Comparison Strictness" and `harness/test_diff_engine.py` |
| How would you roll this out? | [`docs/MIGRATION.md`](MIGRATION.md) |
| What is not done? | [`docs/RISKS.md`](RISKS.md) |
| How was AI used safely? | [`docs/AI_ASSISTANCE.md`](AI_ASSISTANCE.md) |

## Recovery Lines

- If the discussion gets too detailed: "The detail matters, but the pattern is the important part: executable reference, differential oracle, explicit deviations, reversible rollout."
- If asked why not rewrite idiomatically: "The goal was parity first. Structural similarity to C made behavioral drift easier to find."
- If asked why the proxy uses async but the server does not: "They have different jobs. The server preserves the C event-loop shape; the proxy manages concurrent backend traffic."

## See also

- [RISKS.md](RISKS.md) — current gaps and verified strengths
- [MIGRATION.md](MIGRATION.md) — migration guide
- [JOURNEY.md](../JOURNEY.md) — migration case study
- [ROADMAP.md](../ROADMAP.md) — full project roadmap
