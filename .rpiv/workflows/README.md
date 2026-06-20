# Workflows — plan execution loops

Three composable [`@juicesharp/rpiv-workflow`](https://www.npmjs.com/package/@juicesharp/rpiv-workflow)
graphs that string the rpiv skills (`implement`, `validate`, `commit`) into the
full implement → review → fix-or-continue loop, with each step running in its
own fresh Pi session and the plan path as the only thing carried between them.

Run any of them with `/wf <name> <plan-path>`.

| Workflow | What it does | When to use it |
|----------|--------------|----------------|
| [`phase`](phase.ts) | Implement **one** phase, validate-gated with a fix-loop (up to 3 attempts), then commit. | **Default.** Risky phases (crate refactors, `unsafe`/parity changes). Gives a per-phase checkpoint. |
| [`ship`](ship.ts) | Validate an already-implemented plan and commit if it passes. Skips `implement`. | You wrote the code by hand and just want the gate + commit. |
| [`feature`](feature.ts) | Implement **every** phase (one fresh session per phase), then validate + commit. | Fire-and-forget end-to-end. End-gated (validate runs once at the end, not per phase). |

The "skip steps / separate workflows" capability is just picking the right entry
point above. There is no master switch — the three are deliberately independent.

## Usage

```text
# Recommended for this repo's security plan — one phase at a time:
/wf phase .rpiv/artifacts/plans/2026-06-12_16-30-00_security-report.md

# You wrote Phase 2 yourself, just want it checked + committed:
/wf ship  .rpiv/artifacts/plans/2026-06-12_16-30-00_security-report.md

# Run the whole plan end-to-end (coarse; validate only at the very end):
/wf feature .rpiv/artifacts/plans/2026-06-12_16-30-00_security-report.md
```

You do **not** pass a phase number to `phase` — `implement` auto-resumes at the
first unchecked phase in the plan, so re-running `/wf phase <plan>` naturally
walks the plan forward one phase per invocation. That is the managed loop: one
`/wf phase` per phase, each a bounded, validated, committed unit.

## How the loop works (and why no `/new` / handoffs)

You do not need `create-handoff`, `resume-handoff`, or `/new` with these
workflows — the framework provides what those skills were approximating:

- **Fresh session per step** — each stage (and each loop unit) opens its own Pi
  session, so context does not pile up. This is the "new session" property,
  automatic.
- **The plan path is the bridge** — `_shared.ts::planPathCollector` re-emits the
  run's plan path as the rolling primary, so every stage gets
  `/skill:<name> <plan-path>`. The "small understanding to bridge" between
  sessions is just the plan path plus the audited JSONL run state.
- **The review is the gate** — `validate` runs as a `verify` judge (`phase`) or
  a routed `produces` stage (`ship`, `feature`); its `verdict: pass|fail`
  decides branch/continue.
- **The fix loop is built in** — `phase`'s `verify` feeds the validate findings
  back to `implement` and retries up to 3 times before halting (the
  "surface-to-human" signal).

## The fix-or-continue branches

- **`phase`** — pass → `commit` → stop. Fail → retry `implement` with findings
  (up to 3). Still failing → **halt** (run `/skill:revise <plan>` if the issue
  is plan-level, or fix manually and re-run `/wf phase`).
- **`ship`** / **`feature`** — pass → `commit`. Fail → `stop` (working tree
  left as-is; fix and re-run, or switch to `phase` to drive the fix).

## Files

- [`_shared.ts`](_shared.ts) — shared outcomes: `planPathCollector`
  (plan-path-as-handle), `validationOutcome` (collects + parses the validate
  report's `verdict:` frontmatter into `{ pass, findings, reportPath }`), and
  `readPlanPhases` (parses a plan's frontmatter `phases:` array for `feature`).
- [`phase.ts`](phase.ts), [`ship.ts`](ship.ts), [`feature.ts`](feature.ts) — the
  three workflows.

## Design notes

- **No external schema dependency.** Routing reads `state.named["validation"]`
  via `defineRoute({ readsData: false })`, and `verify`'s `done` reads the
  parsed verdict directly — so no `outputSchema` / `@sinclair/typebox` is
  required. Everything is self-contained.
- **Why `phase` uses `verify` and `feature` uses `iterate`.** The DSL forbids
  combining a loop (`iterate`) with `verify` on one stage. `phase` prioritizes
  the per-phase fix-loop (needs `verify`); `feature` prioritizes
  multi-phase-in-one-run (needs `iterate`) and accepts coarser end-gating.
  That is the real trade-off between them, not a limitation to "fix."
- **`feature` validate is end-only.** If an early phase is subtly wrong, later
  phases build on it and you discover it at the end. For the security plan,
  prefer `phase` for Phase 1 (CVE inventory — foundational) and Phase 3
  (the `thttpd-auth` extraction — parity-sensitive); `feature` is fine for the
  independent infrastructure phases (4, 5, 7).

## Validated

All three workflows pass `validateWorkflow()` (0 issues). Re-check after edits:

```bash
# jiti is pi's own TS loader
NODE_PATH=/home/burket/.pi/agent/npm/node_modules \
  /home/burket/.pi/agent/npm/node_modules/.bin/jiti <(printf '%s\n' \
  'import { validateWorkflow } from "@juicesharp/rpiv-workflow";' \
  'import phase from "./.rpiv/workflows/phase.ts";' \
  'import ship from "./.rpiv/workflows/ship.ts";' \
  'import feature from "./.rpiv/workflows/feature.ts";' \
  'for (const w of [phase,ship,feature] as any[])' \
  '  console.log(w.name, validateWorkflow(w).filter(i=>i.severity==="error").length, "errors");')
```

## What is NOT validated here (needs a live dry-run)

- **Skill arg plumbing at runtime** — `validateWorkflow()` checks graph
  structure, not that `/skill:validate <plan-path>` behaves as expected when
  dispatched by the judge. Dry-run on Phase 1 first; if the validate judge
  mis-targets the plan, the fix is in `_shared.ts::planPathCollector`.
- **The verdict frontmatter regex** — assumes validate keeps emitting
  `verdict: pass|fail`. If a future validate skill changes its report shape,
  update `verdictParser` in `_shared.ts`.
