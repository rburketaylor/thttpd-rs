/**
 * `feature` — implement EVERY phase of a plan end-to-end, then validate + commit.
 *
 * One fresh session per phase (via `iterate` over the plan's frontmatter
 * `phases:` array), then a single end-of-run `validate` gate and a commit.
 *
 *   implement[phase 1] → implement[phase 2] → … → implement[phase N] → validate → commit | stop
 *
 * Trade-off vs `phase`: `feature` is fire-and-forget end-to-end, but the
 * validate gate runs ONCE at the end (coarse), not per phase. If an early
 * phase is subtly wrong, later phases build on it and you only find out at the
 * end. For risky phases (crate refactors, anything touching `unsafe`/parity),
 * prefer `/wf phase <plan>` per phase for the per-phase fix-loop.
 *
 * Each phase session is fresh (the loop driver opens a new Pi session per
 * unit), so context does NOT pile up across phases — the plan path (re-emitted
 * by `planPathCollector`) is the only thing that carries forward.
 *
 * Run: `/wf feature .rpiv/artifacts/plans/2026-06-12_16-30-00_security-report.md`
 */
import {
  defineWorkflow,
  produces,
  acts,
  iterate,
  defineRoute,
  gitCommitOutcome,
  type IterateContext,
  type Unit,
  type EdgeContext,
} from "@juicesharp/rpiv-workflow";
import { planPathCollector, validationOutcome, readPlanPhases } from "./_shared.js";

/**
 * Pull-style phase loop. `next()` is called per unit, fed the accumulated
 * prior units; returning null terminates. The plan path comes from the frozen
 * run input; phases are read from its frontmatter.
 */
const PHASE_ITERATE = iterate({
  source: "impl", // introspection hint: units split from the "impl" channel
  max: 20, // hard ceiling; also clamped by the run-wide maxIterations (default 32)
  next: ({ state, accumulated, cwd }: IterateContext): Unit | null => {
    const planPath = (state.originalInput || "").trim().split(/\s+/)[0];
    if (!planPath) return null;
    const phases = readPlanPhases(planPath, cwd);
    const i = accumulated.length;
    if (i >= phases.length) return null; // every phase implemented → terminate
    const p = phases[i]!;
    return {
      prompt: `${planPath} Phase ${p.n}: ${p.title}`,
      label: `phase ${i + 1}/${phases.length}`,
      id: `phase-${p.n}`, // stable audit key (survives a reworded label on resume)
    };
  },
});

/** Routes on the validate report's verdict, read from the named publish slot. */
const routeOnVerdict = defineRoute(
  ["commit", "stop"],
  ({ state }: EdgeContext) => {
    const verdict = state.named["validation"]?.at(-1)?.data as { pass?: boolean } | undefined;
    return verdict?.pass ? "commit" : "stop";
  },
  { readsData: false },
);

export default defineWorkflow({
  name: "feature",
  description:
    "Implement every phase of a plan (one fresh session per phase), then validate + commit. Coarse end-to-end (end-gated). Run input = plan path.",
  start: "implement",
  stages: {
    // iterate requires `produces` + a named outcome. planPathCollector keeps
    // the plan path as the rolling primary so the downstream `validate` stage
    // receives `/skill:validate <plan-path>`.
    implement: produces({
      skill: "implement",
      outcome: { name: "impl", collector: planPathCollector },
      loop: PHASE_ITERATE,
    }),
    validate: produces({ skill: "validate", outcome: validationOutcome }),
    commit: acts({ skill: "commit", outcome: gitCommitOutcome }),
  },
  edges: {
    implement: "validate",
    validate: routeOnVerdict,
    commit: "stop",
  },
});
