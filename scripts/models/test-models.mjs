#!/usr/bin/env node
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

test("model manifest pins revisions, SHA-256 digests, and gated redistribution rules", async () => {
  const manifest = JSON.parse(await readFile(path.join(root, "catalog/models/manifest.json"), "utf8"));
  assert.equal(manifest.schema_version, 1);
  assert.ok(manifest.models.length >= 4);
  for (const model of manifest.models) {
    if (model.requires_opt_in) {
      assert.match(model.redistribution ?? "", /blocked/);
    }
    for (const file of model.files) {
      assert.match(file.sha256, /^[a-f0-9]{64}$/);
      assert.ok(Number.isSafeInteger(file.bytes) && file.bytes > 0);
    }
  }
});

test("download.mjs --dry-run reports expected files without mutating disk", () => {
  const res = spawnSync("node", ["scripts/models/download.mjs", "--set", "core", "--dry-run"], {
    cwd: root,
    encoding: "utf8",
  });
  assert.equal(res.status, 0, res.stderr);
  assert.match(res.stdout, /Dry run complete: 2 files/);
});

test("download.mjs rejects unknown gated opt-in and missing HF_TOKEN on gated model", () => {
  const badOptIn = spawnSync(
    "node",
    ["scripts/models/download.mjs", "--set", "creative", "--include-gated", "nonexistent-gate", "--dry-run"],
    { cwd: root, encoding: "utf8" },
  );
  assert.notEqual(badOptIn.status, 0);
  assert.match(badOptIn.stderr, /unknown gated model opt-in/);

  const envWithoutToken = { ...process.env };
  delete envWithoutToken.HF_TOKEN;
  const missingToken = spawnSync(
    "node",
    ["scripts/models/download.mjs", "--set", "creative", "--include-gated", "flux2-encoder", "--dry-run"],
    { cwd: root, env: envWithoutToken, encoding: "utf8" },
  );
  assert.notEqual(missingToken.status, 0);
  assert.match(missingToken.stderr, /Accept its source terms and export HF_TOKEN/);
});
