#!/usr/bin/env node
import { readdir, readFile, stat } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");
const ignored = new Set([".git", "target", "shared/runtime", "shared/chat/home", "shared/models", "runs", "evidence"]);
const sourceExtensions = new Set([".rs", ".mjs", ".py"]);
const problems = [];

async function walk(relative = "") {
  const absolute = path.join(root, relative);
  for (const entry of await readdir(absolute, { withFileTypes: true })) {
    const child = path.join(relative, entry.name);
    if (entry.isDirectory() && ["target", "__pycache__"].includes(entry.name)) continue;
    if ([...ignored].some((prefix) => child === prefix || child.startsWith(`${prefix}/`))) continue;
    if (entry.isDirectory()) await walk(child);
    else await inspect(child);
  }
}

async function inspect(relative) {
  const absolute = path.join(root, relative);
  const extension = path.extname(relative);
  const textEligible = new Set([".rs", ".mjs", ".py", ".md", ".json", ".toml", ".sh", ".yml", ".yaml", ".html"]);
  if (!textEligible.has(extension) && !["LICENSE", "NOTICE"].includes(path.basename(relative))) return;
  const text = await readFile(absolute, "utf8");
  if (sourceExtensions.has(extension)) {
    const lines = text.split(/\r?\n/).length;
    if (lines > 700) problems.push(`${relative}: ${lines} lines exceeds the 700-line source ceiling`);
  }
  const maintainerPath = ["", "Users", "chris.rijos", ""].join("/");
  if (text.includes(maintainerPath)) problems.push(`${relative}: contains a maintainer-specific absolute path`);
  const privateProduct = ["prae", "co"].join("");
  if (text.toLowerCase().includes(privateProduct)) problems.push(`${relative}: contains a private downstream product name`);
  const legacyWord = ["ae", "gis"].join("");
  if (text.toLowerCase().includes(legacyWord)) problems.push(`${relative}: contains deprecated legacy nomenclature; use ISA / military callsign designations`);
  if (/-----BEGIN (?:RSA |EC |OPENSSH )?PRIVATE KEY-----/.test(text)) problems.push(`${relative}: contains private-key material`);
  if (extension === ".json") {
    try { JSON.parse(text); } catch (error) { problems.push(`${relative}: invalid JSON (${error.message})`); }
  }
}

await walk();
for (const relative of ["install.sh", "bin/agents-chat", "shared/bin/run-agent.sh", "shared/bin/run-image.sh", "shared/bin/run-video.sh", "shared/bin/run-mcp.sh"]) {
  const mode = (await stat(path.join(root, relative))).mode;
  if ((mode & 0o111) === 0) problems.push(`${relative}: launcher is not executable`);
}
const manifest = JSON.parse(await readFile(path.join(root, "catalog/models/manifest.json"), "utf8"));
for (const model of manifest.models) {
  if (model.requires_opt_in && !model.redistribution?.includes("blocked")) problems.push(`${model.id}: gated model lacks a blocked redistribution rule`);
  for (const file of model.files) {
    if (!/^[a-f0-9]{64}$/.test(file.sha256)) problems.push(`${model.id}/${file.remote}: invalid SHA-256`);
    if (!Number.isSafeInteger(file.bytes) || file.bytes <= 0) problems.push(`${model.id}/${file.remote}: invalid byte count`);
  }
}
const vendor = JSON.parse(await readFile(path.join(root, "catalog/vendor/mlx-wan.json"), "utf8"));
if (!/^[a-f0-9]{40}$/.test(vendor.source_revision ?? "")) problems.push("mlx-wan provenance: exact 40-character upstream revision is missing");
const mirrorPlan = JSON.parse(await readFile(path.join(root, "catalog/models/mirror-plan.json"), "utf8"));
if (mirrorPlan.status !== "proposed_only") problems.push("mirror plan: must remain proposed_only until Hugging Face publication is observed");
for (const repository of mirrorPlan.repositories) {
  try { await stat(path.join(root, repository.card)); } catch { problems.push(`${repository.name}: missing proposed Hugging Face model card`); }
}
const report = await readFile(path.join(root, "reports/syndicate-recursive-harness-report.html"), "utf8");
for (const id of ["top", "inception", "research", "system", "design", "walk", "delivery", "verify", "gtm", "operate", "register", "decide", "log"]) {
  if (!report.includes(`id="${id}"`)) problems.push(`report: missing required section id ${id}`);
}
if (problems.length) {
  console.error(problems.map((problem) => `✗ ${problem}`).join("\n"));
  process.exit(1);
}
console.log(`Repository verification passed: ${manifest.models.length} model entries, required report sections, executable launchers, privacy scan, JSON parse, and source-size ceiling.`);
