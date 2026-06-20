/**
 * `ship` — validate an already-implemented plan and commit if it passes.
 *
 * Use this when you implemented (or partially implemented) the work yourself
 * and only want the gate + commit. It skips `implement` entirely:
 *   1. `/skill:validate <plan-path>` checks the working tree against the plan.
 *   2. On pass → `/skill:commit`. On fail → stop (the working tree is left as-is
 *      for you to fix; re-run `/wf ship <plan>` once it passes, or `/wf phase`
 *      to drive the fix automatically).
 *
 * This is the "skip the implement step" workflow — one of the separate,
 * composable entry points (phase / ship / feature) you can pick from depending
 * on which steps you want to run.
 *
 * Run: `/wf ship .rpiv/artifacts/plans/2026-06-12_16-30-00_security-report.md`
 */
import {
  defineWorkflow,
  produces,
  acts,
  defineRoute,
  gitCommitOutcome,
  type EdgeContext,
} from "@juicesharp/rpiv-workflow";
import { validationOutcome } from "./_shared.js";

/** Routes on the validate report's verdict, read from the named publish slot. */
const routeOnVerdict = defineRoute(
  ["commit", "stop"],
  ({ state }: EdgeContext) => {
    const verdict = state.named["validation"]?.at(-1)?.data as { pass?: boolean } | undefined;
    return verdict?.pass ? "commit" : "stop";
  },
  { readsData: false }, // state-only route → no outputSchema / typebox dependency
);

export default defineWorkflow({
  name: "ship",
  description:
    "Validate an already-implemented plan and commit if it passes (skips implement). Run input = plan path.",
  start: "validate",
  stages: {
    validate: produces({ skill: "validate", outcome: validationOutcome }),
    commit: acts({ skill: "commit", outcome: gitCommitOutcome }),
  },
  edges: {
    validate: routeOnVerdict,
    commit: "stop",
  },
});
