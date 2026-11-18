#!/usr/bin/env node
import { createHash } from "node:crypto";
import { createReadStream, createWriteStream } from "node:fs";
import { mkdir, readFile, rename, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { Readable } from "node:stream";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const manifestPath = path.join(root, "catalog/models/manifest.json");
const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
const setIndex = process.argv.indexOf("--set");
const selected = setIndex >= 0 ? process.argv[setIndex + 1] : "core";
const dryRun = process.argv.includes("--dry-run");
const optInIndex = process.argv.indexOf("--include-gated");
const optedIn = new Set(optInIndex >= 0 ? (process.argv[optInIndex + 1] ?? "").split(",").filter(Boolean) : []);
if (!new Set(["core", "creative", "all"]).has(selected)) {
  throw new Error("--set must be core, creative, or all");
}

function formatBytes(bytes) {
  return bytes >= 1024 ** 3
    ? `${(bytes / 1024 ** 3).toFixed(2)} GiB`
    : `${(bytes / 1024 ** 2).toFixed(1)} MiB`;
}

async function digest(file) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest("hex");
}

function encodedPath(value) {
  return value.split("/").map(encodeURIComponent).join("/");
}

async function valid(file, expectedBytes, expectedHash) {
  try {
    const metadata = await stat(file);
    if (metadata.size !== expectedBytes) return false;
    return (await digest(file)) === expectedHash;
  } catch {
    return false;
  }
}

async function download(model, file) {
  const repo = file.repo ?? model.source_repo;
  const revision = file.revision ?? model.revision;
  const target = path.join(root, file.local);
  const sizeLabel = formatBytes(file.bytes);
  if (await valid(target, file.bytes, file.sha256)) {
    console.log(`✓ ${file.local} (${sizeLabel}) already verified`);
    return { ...file, repo, revision, status: "already_verified" };
  }
  if (dryRun) {
    console.log(`• [dry-run] would download ${repo}@${revision}:${file.remote} -> ${file.local} (${sizeLabel})`);
    return { ...file, repo, revision, status: "dry_run_pending" };
  }
  await mkdir(path.dirname(target), { recursive: true });
  const partial = `${target}.part`;
  try {
    const existing = await stat(target);
    if (existing.isFile()) await rename(target, `${target}.invalid-${Date.now()}`);
  } catch {}
  const url = `https://huggingface.co/${repo}/resolve/${revision}/${encodedPath(file.remote)}`;
  console.log(`↓ ${repo}@${revision}:${file.remote} (${sizeLabel})`);
  let lastError;
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    try {
      let offset = 0;
      try { offset = (await stat(partial)).size; } catch {}
      if (offset > file.bytes) {
        await rename(partial, `${partial}.invalid-${Date.now()}`);
        offset = 0;
      } else if (offset === file.bytes) {
        const actual = await digest(partial);
        if (actual === file.sha256) {
          await rename(partial, target);
          return { ...file, repo, revision, status: "resumed_and_verified", attempts: attempt };
        }
        await rename(partial, `${partial}.invalid-${Date.now()}`);
        offset = 0;
      }
      const headers = {};
      if (process.env.HF_TOKEN) headers.Authorization = `Bearer ${process.env.HF_TOKEN}`;
      if (offset > 0) headers.Range = `bytes=${offset}-`;
      const response = await fetch(url, { headers, redirect: "follow" });
      if (!response.ok || !response.body) {
        const gated = model.access?.startsWith("gated") ? " Accept the source terms and export HF_TOKEN, then retry." : "";
        throw new Error(`HTTP ${response.status} downloading ${repo}:${file.remote}.${gated}`);
      }
      const append = offset > 0 && response.status === 206;
      await pipeline(Readable.fromWeb(response.body), createWriteStream(partial, { mode: 0o600, flags: append ? "a" : "w" }));
      const metadata = await stat(partial);
      if (metadata.size !== file.bytes) {
        throw new Error(`size mismatch for ${file.remote}: ${metadata.size}/${file.bytes}`);
      }
      const actual = await digest(partial);
      if (actual !== file.sha256) {
        await rename(partial, `${partial}.invalid-${Date.now()}`);
        throw new Error(`SHA-256 mismatch for ${file.remote}: ${actual}`);
      }
      await rename(partial, target);
      return { ...file, repo, revision, status: "downloaded_and_verified", attempts: attempt };
    } catch (error) {
      lastError = error;
      if (attempt < 3) console.warn(`  retry ${attempt}/2 after: ${error.message}`);
    }
  }
  throw lastError;
}

const models = manifest.models.filter((model) => {
  const selectedBySet = selected === "all" || model.set === selected;
  if (model.requires_opt_in) return optedIn.has(model.requires_opt_in);
  return selectedBySet;
});
for (const value of optedIn) {
  if (!manifest.models.some((model) => model.requires_opt_in === value)) throw new Error(`unknown gated model opt-in: ${value}`);
}
if (models.some((model) => model.requires_opt_in) && !process.env.HF_TOKEN) {
  throw new Error("A gated model was explicitly selected. Accept its source terms and export HF_TOKEN before retrying.");
}
const results = [];
for (const model of models) {
  for (const file of model.files) results.push(await download(model, file));
}
if (dryRun) {
  const totalBytes = results.reduce((sum, item) => sum + item.bytes, 0);
  console.log(`Dry run complete: ${results.length} files (${formatBytes(totalBytes)}) for set ${selected}.`);
  process.exit(0);
}
const receiptDir = path.join(root, "evidence/qualification");
await mkdir(receiptDir, { recursive: true });
const receipt = {
  generated_at: new Date().toISOString(),
  selected_set: selected,
  files: results,
  integrity_verified: true,
  credentials_recorded: false
};
await writeFile(path.join(receiptDir, `model-download-${selected}.json`), `${JSON.stringify(receipt, null, 2)}\n`);
console.log(`Verified ${results.length} model files for set ${selected}.`);

