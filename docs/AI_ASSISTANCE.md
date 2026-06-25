# AI Assistance on This Project

This project was built with AI assistance throughout. The interesting part is
not the tool — it is the discipline around it: how the work was structured so
AI-generated code could be trusted, how failures were caught, and where human
judgment stayed in the loop.

The first AI-assisted attempt at this port reported itself complete. It did
not work. Everything below was a response to that failure.

## The failure that shaped everything

The first pass reported all twenty-two implementation phases complete. Every
gate was structural — a file existed, it compiled, a test was collected — and
the implementation met the letter of each. The Rust unit tests passed. The
server did not answer a single request. The event loop dispatched nothing.

This is the failure mode AI assistance is most prone to: satisfying the
specification as written while building nothing that works. A gate that can be
checked mechanically will be met mechanically.

The fix was not more code, but better gates. Every gate was restated as an
observable behavior: a real request returns the file body, the C reference
serves on this port, the harness asserts on a real response. From then on,
"done" meant "behaves correctly," never "looks complete."

## The C binary is the oracle

The original C server was kept in the repository as an executable
specification, and its behavior was captured *before* any Rust was written.
Over a hundred representative requests were replayed against both servers, and
a comparison engine produced a verdict on each — status, headers, body,
connection outcome.

This turned AI output from something to review into something to test. When
the Rust server disagreed with the C server, it did not matter how plausible
the code looked. It was wrong, and the diff said where.

The three hardest bugs this caught are exactly the kind review misses:

- **A CGI pipe deadlock.** A request with no body length left the CGI child's
  stdin open; the child blocked reading it, the server blocked reading the
  child's output.
- **A negative body length.** The header `"-1"` wrapped to an effectively
  unbounded size; the C server reads the same value but treats negative as
  "not specified."
- **A parser that forgot its state.** The request parser reset on every
  network read instead of resuming. Invisible for a request that arrives at
  once; fatal for one that trickles in byte by byte.

All three are edge cases in *how input is delivered or sized*, not what it
contains. No unit test that hands over a well-formed request in one piece
finds them. All three surface immediately under a harness that replays the
same bytes against an implementation that got them right thirty years ago.

## The oracle had to be tested too

The comparison engine — the thing trusted to detect drift — had a bug of its
own. In its lenient mode it marked the response body as matching
unconditionally, so two responses with different bodies passed as long as the
headers lined up. A stretch of "passing" tests had been partly illusory.

The lesson: a tool whose only job is to catch mistakes has to be tested for
its own mistakes. The engine now hashes the normalized body and fails on any
real difference, backed by its own suite of sixty-three tests. The 105 parity
tests are only worth what those 63 tests say they are worth.

## Directing the work

The loop was structured, not ad hoc. When a differential test failed:
classify the mismatch, trace it to the Rust code path, hand the diff to a
repair pass, recompile, retest — capped at five cycles, then a human takes
over. Every repair was written down. The cap matters: an assistant that keeps
retrying past its understanding produces worse results, not better.

Larger plans went through independent review. The migration proxy plan was
reviewed three times by separate model passes; the third found four compile
blockers and nine concerns the first two missed — including a borrowed value
moved into an asynchronous task, which the borrow checker would have rejected.
Each model has different blind spots; that is the point of using more than
one.

## What stayed human

The decisions AI is weakest at — the ones with no single right answer — were
written down, not left implicit:

- Preserve the original single-threaded event loop rather than rewrite
  idiomatically, because structural similarity to the C original is what makes
  the differential tests meaningful.
- One runtime for the server, another for the proxy, because the workloads are
  fundamentally different.
- Publish a list of known gaps rather than claim compatibility, because an
  honest register is what makes the passing tests trustworthy.

These live as an architecture decision record and a deviations register.

## The principle

The value is not in the generation, it is in the verification. Code that
writes itself is now cheap; code you can trust is not. What paid off here was
not better prompts — it was a reference to check against, a harness that
compared behavior automatically, a policy for when to stop retrying, and the
habit of testing the tools that test the code.

## Where to see each claim

- The failure and the three bugs: [`JOURNEY.md`](../JOURNEY.md)
- The comparison engine and its tests: [`harness/diff_engine.py`](../harness/diff_engine.py), [`harness/test_diff_engine.py`](../harness/test_diff_engine.py)
- The 105 differential scenarios: [`harness/tests/test_differential.py`](../harness/tests/test_differential.py)
- Known gaps, each with a disposition: [`RISKS.md`](RISKS.md)
- The runtime decision: [`ADR-0002-async-runtime-split.md`](ADR-0002-async-runtime-split.md)
- Planning records and model reviews: [`.rpiv/artifacts/`](../.rpiv/artifacts/)
