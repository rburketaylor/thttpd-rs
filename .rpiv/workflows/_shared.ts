/**
 * Shared outcome helpers for the feature/phase/ship workflows.
 *
 * These are inlined into a single module so each workflow file stays
 * self-contained while avoiding duplication. Loaded by pi via jiti
 * (TypeScript, no build step) the same way the workflow files are.
 */
import {
  defineCollector,
  defineParser,
  transcriptPathCollector,
  fs,
  type Outcome,
} from "@juicesharp/rpiv-workflow";
import { readFileSync } from "node:fs";
import { join } from "node:path";

/**
 * Emits the run's plan path as the stage's primary handle.
 *
 * The `/wf <workflow> <plan-path>` run input is frozen on `state.originalInput`
 * for the whole run. We take the first whitespace-delimited token as the plan
 * path and re-emit it as an `fs` artifact so downstream stages (and the verify
 * judge) receive `/skill:<name> <plan-path>` rather than some intermediate
 * diff artifact. This is what lets the `validate` judge/slide reliably target
 * the plan being implemented.
 */
export const planPathCollector = defineCollector({
  collect: (ctx) => {
    const planPath = (ctx.state.originalInput || "").trim().split(/\s+/)[0];
    if (!planPath) {
      return {
        kind: "fatal",
        message:
          "Run input must be a plan path, e.g. `/wf phase .rpiv/artifacts/plans/<file>.md`",
      };
    }
    return { kind: "ok", artifacts: [{ handle: fs(planPath), role: "primary" }] };
  },
});

/**
 * Collects the path of the report the `validate` skill writes.
 * validate prints `Validation written to: .rpiv/artifacts/validation/<slug>_<topic>.md`
 * at the end of its run; this collector scans the transcript for that path.
 */
const VERDICT_PATH_RE = /\.rpiv\/artifacts\/validation\/[^\s`"'<>]+\.md/g;
export const reportPathCollector = transcriptPathCollector({ pattern: VERDICT_PATH_RE });

/**
 * Parses a validate report's YAML frontmatter into a typed verdict.
 *
 * validate's report carries `verdict: pass | fail` in its frontmatter. We read
 * the collected report file and surface `{ pass, findings, reportPath }` so the
 * verify `done` predicate and `feedForward` can act on it, and so route
 * predicates can branch on `state.named["validation"]`.
 */
export const verdictParser = defineParser({
  parse: (ctx) => {
    const art = ctx.artifacts[0];
    if (!art || art.handle.kind !== "fs") {
      return { kind: "fatal", message: "no validation report path was collected" };
    }
    let body: string;
    try {
      body = readFileSync(join(ctx.cwd, art.handle.path), "utf8");
    } catch (e) {
      return { kind: "fatal", message: `could not read validation report ${art.handle.path}: ${(e as Error).message}` };
    }
    const m = /^verdict:\s*(pass|fail)\s*$/im.exec(body);
    const verdict = m?.[1]?.toLowerCase();
    if (verdict !== "pass" && verdict !== "fail") {
      return { kind: "fatal", message: `could not parse a pass/fail verdict from ${art.handle.path}` };
    }
    // Findings = the body after the frontmatter, capped so feedForward stays bounded.
    const afterFm = body.split(/^\.\.\.$/m)[1] ?? body;
    const findings = afterFm.split("\n").slice(0, 150).join("\n").trim();
    return {
      kind: "ok",
      payload: {
        kind: "validation",
        data: { pass: verdict === "pass", findings, reportPath: art.handle.path },
      },
    };
  },
});

/** Outcome used by every `validate` invocation (judge or standalone stage). */
export const validationOutcome: Outcome = {
  name: "validation",
  collector: reportPathCollector,
  parser: verdictParser,
};

/** Plan-frontmatter phase record (matches the `phases: [{ n, title }, ...]` shape). */
export interface PhaseRecord {
  n: number;
  title: string;
}

/**
 * Reads the `phases:` array out of a plan's YAML frontmatter.
 *
 * Tolerant of quoted or unquoted titles and of the inline-flow
 * `{ n: 1, title: ... }` layout this project's plans use. Used by the
 * `feature` workflow's iterate loop to emit one unit per phase.
 */
export function readPlanPhases(planPath: string, cwd: string): PhaseRecord[] {
  let body: string;
  try {
    body = readFileSync(join(cwd, planPath), "utf8");
  } catch {
    return [];
  }
  const parts = body.split(/^---\s*$/m);
  const fm = parts.length >= 3 ? parts[1] ?? "" : body;
  const phases: PhaseRecord[] = [];
  // Match: `  - { n: 1, title: Some Title }` or `  - { n: 1, title: "Some Title" }`
  const re = /-\s*\{\s*n:\s*(\d+)\s*,\s*title:\s*(?:")?([^"}]+?)(?:")?\s*\}/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(fm)) !== null) {
    phases.push({ n: Number(m[1]), title: m[2]!.trim() });
  }
  return phases;
}
