/**
 * `phase` — implement ONE phase of a plan with a validate-gated fix-loop, then commit.
 *
 * This is the workhorse. Each `/wf phase <plan-path>` run:
 *   1. `/skill:implement <plan-path>` (fresh session) — implement auto-resumes
 *      at the first unchecked phase, so you do NOT pass a phase number.
 *   2. `/skill:validate <plan-path>` grades the result (the `verify` judge).
 *   3. On fail: the validate findings are fed back and implement is retried,
 *      up to `max: 3` attempts. On pass (or exhaustion) it advances.
 *   4. `/skill:commit` groups the work into atomic commits.
 *
 * If validate still fails after 3 attempts the workflow HALTS — that is the
 * "surface to a human" signal (run `/skill:revise <plan>` if the issue is
 * plan-level, or fix manually and re-run).
 *
 * This gives the per-phase checkpoint + fix-loop you want for the risky
 * phases (e.g. Phase 3's crate refactor in the security plan). For a whole
 * plan in one shot, use `feature`; to validate+commit code you wrote by hand,
 * use `ship`.
 *
 * Run: `/wf phase .rpiv/artifacts/plans/2026-06-12_16-30-00_security-report.md`
 */
import {
  defineWorkflow,
  produces,
  acts,
  judge,
  verify,
  gitCommitOutcome,
  type Output,
} from "@juicesharp/rpiv-workflow";
import { planPathCollector, validationOutcome } from "./_shared.js";

export default defineWorkflow({
  name: "phase",
  description:
    "Implement one phase of a plan (validate-gated, up to 3 fix attempts) then commit. Run input = plan path.",
  start: "implement",
  stages: {
    // `produces` so it can host `verify`. The planPathCollector re-emits the
    // plan path as the primary handle, so the validate judge is dispatched as
    // `/skill:validate <plan-path>` (not a diff artifact).
    implement: produces({
      skill: "implement",
      outcome: { name: "impl", collector: planPathCollector },
      verify: verify({
        judge: judge({ skill: "validate", outcome: validationOutcome }),
        done: (v: Output) => (v.data as { pass?: boolean }).pass === true,
        feedForward: ({ verdict }) => {
          const d = verdict.data as { reportPath?: string; findings?: string };
          return (
            `The previous implement attempt did not pass validation. ` +
            `Open the report at ${d.reportPath ?? "<.rpiv/artifacts/validation/…>"}, ` +
            `address every finding, then re-implement:\n\n${d.findings ?? ""}`
          );
        },
        max: 3,
      }),
    }),
    commit: acts({ skill: "commit", outcome: gitCommitOutcome }),
  },
  edges: {
    // verify advances the stage only when `done` passes (or after `max`
    // attempts, at which point it halts instead of reaching this edge).
    implement: "commit",
    commit: "stop",
  },
});
