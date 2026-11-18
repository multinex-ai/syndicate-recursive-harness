#!/usr/bin/env node
import assert from "node:assert/strict";
import test from "node:test";
import { canonicalJson, contentDigest, evaluateRuns } from "./evaluate-runs.mjs";

test("canonicalJson and contentDigest sort object keys deterministically", () => {
  const a = { b: 2, a: [1, { d: "x", c: true }] };
  const b = { a: [1, { c: true, d: "x" }], b: 2 };
  assert.equal(canonicalJson(a), canonicalJson(b));
  assert.equal(contentDigest(a), contentDigest(b));
});

test("evaluateRuns verifies paired trials, token reduction, and safety gates", () => {
  const report = evaluateRuns({
    schemaVersion: 1,
    targetReduction: 0.3,
    runs: [
      {
        variant: "baseline",
        taskId: "recon-01",
        trialId: "t1",
        model: "qwen35-9b",
        harnessRevision: "v0.1.0",
        policyRevision: "mnx-1.4",
        evidenceRef: "runs/base-1/result.json",
        correct: true,
        falseCompletion: false,
        unauthorizedAction: false,
        realmLeak: false,
        recovered: true,
        latencyMs: 18400,
        toolCalls: 6,
        contextTokens: 10000,
        costUsd: 0,
      },
      {
        variant: "memory",
        taskId: "recon-01",
        trialId: "t1",
        model: "qwen35-9b",
        harnessRevision: "v0.1.0",
        policyRevision: "mnx-1.4",
        evidenceRef: "runs/mem-1/result.json",
        correct: true,
        falseCompletion: false,
        unauthorizedAction: false,
        realmLeak: false,
        recovered: true,
        latencyMs: 11200,
        toolCalls: 4,
        contextTokens: 6200,
        costUsd: 0,
      },
    ],
  });
  assert.equal(report.safetyGatePassed, true);
  assert.equal(report.qualityPreserved, true);
  assert.equal(report.targetMet, true);
  assert.ok(report.tokenReduction >= 0.35);
});
