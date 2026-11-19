#!/usr/bin/env node
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const manifest = JSON.parse(await readFile(path.join(root, "catalog/models/manifest.json"), "utf8"));
const setIndex = process.argv.indexOf("--set");
const selected = setIndex >= 0 ? process.argv[setIndex + 1] : "core";
if (!new Set(["core", "creative", "all"]).has(selected)) throw new Error("--set must be core, creative, or all");

async function sha256(file) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest("hex");
}

const models = manifest.models.filter((model) =>
  !model.requires_opt_in && (selected === "all" || model.set === selected));
const tasks = models.flatMap((model) =>
  model.files.map((expected) => async () => {
    const target = path.join(root, expected.local);
    try {
      const metadata = await stat(target);
      const digest = metadata.size === expected.bytes ? await sha256(target) : "not-computed";
      const ok = metadata.size === expected.bytes && digest === expected.sha256;
      return { model: model.id, file: expected.local, ok, bytes: metadata.size, sha256: digest };
    } catch (error) {
      return { model: model.id, file: expected.local, ok: false, error: error.code ?? error.message };
    }
  }),
);
const results = await Promise.all(tasks.map((run) => run()));
for (const result of results) {
  console.log(`${result.ok ? "✓" : "✗"} ${result.file}`);
}
const healthy = results.every((result) => result.ok);
const blocked = models.filter((model) => model.runtime_status?.startsWith("blocked")).map((model) => ({ id: model.id, reason: model.runtime_status }));
console.log(JSON.stringify({ selected_set: selected, files_verified: results.filter((item) => item.ok).length, files_expected: results.length, integrity_verified: healthy, runtime_blocked: blocked }, null, 2));
if (!healthy) process.exitCode = 1;
