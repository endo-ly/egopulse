#!/usr/bin/env node
// Regenerates src/llm/model_catalog.json from pi's bundled provider data.
//
// pi publishes the context window and output limit of every model it knows in
// `@earendil-works/pi-ai/dist/providers/data/*.json` (MIT). EgoPulse keeps only
// the two numeric limits plus the provider/model identity, so the runtime never
// parses pi's full provider descriptions.
//
// Usage:
//   node scripts/import-model-catalog.mjs [data-dir]
//
// Without an argument the script looks for a global install:
//   npm install -g @earendil-works/pi-coding-agent
//   node scripts/import-model-catalog.mjs
//
// The data directory must contain the per-provider JSON files. Set
// PI_AI_DATA_DIR to use another checkout. Replacing the generated file is the
// whole update: no model entries are maintained by hand.

import { execFileSync } from "node:child_process";
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const outputPath = join(repositoryRoot, "src", "llm", "model_catalog.json");

// Dynamic routing models have no fixed destination, so a single context window
// cannot describe them. They stay unknown and use the custom-model defaults.
const DYNAMIC_MODELS = new Set(["openrouter/auto"]);

function globalRoot() {
  try {
    return execFileSync("npm", ["root", "-g"], { encoding: "utf8" }).trim();
  } catch {
    return undefined;
  }
}

function defaultDataDir() {
  const root = globalRoot();
  if (!root) return undefined;
  const candidates = [
    join(
      root,
      "@earendil-works/pi-coding-agent/node_modules/@earendil-works/pi-ai/dist/providers/data",
    ),
    join(root, "@earendil-works/pi-ai/dist/providers/data"),
  ];
  return candidates.find((candidate) => existsSync(candidate));
}

function resolveDataDir() {
  const explicit = process.argv[2] ?? process.env.PI_AI_DATA_DIR;
  const dataDir = explicit ? resolve(explicit) : defaultDataDir();
  if (!dataDir || !existsSync(dataDir)) {
    console.error(
      "provider data directory not found; pass it explicitly or install pi globally:\n" +
        "  node scripts/import-model-catalog.mjs /path/to/pi-ai/dist/providers/data",
    );
    process.exit(1);
  }
  return dataDir;
}

function readEntries(dataDir) {
  /** @type {Map<string, [string, string, number, number]>} */
  const entries = new Map();
  for (const file of readdirSync(dataDir).filter((name) => name.endsWith(".json")).sort()) {
    const document = JSON.parse(readFileSync(join(dataDir, file), "utf8"));
    for (const section of Object.values(document)) {
      if (section === null || typeof section !== "object") continue;
      for (const model of Object.values(section)) {
        if (model === null || typeof model !== "object" || model.type !== "chat") continue;
        const provider = model.provider;
        const id = model.id;
        const contextWindow = model.contextWindow;
        const maxTokens = model.maxTokens;
        if (
          typeof provider !== "string" ||
          typeof id !== "string" ||
          !Number.isInteger(contextWindow) ||
          !Number.isInteger(maxTokens) ||
          contextWindow <= 0 ||
          maxTokens <= 0
        ) {
          continue;
        }
        if (DYNAMIC_MODELS.has(id)) continue;
        const key = `${provider}\u0000${id}`;
        const previous = entries.get(key);
        if (
          previous &&
          (previous[2] !== contextWindow || previous[3] !== maxTokens)
        ) {
          throw new Error(
            `conflicting limits for ${provider}/${id}: ` +
              `${previous[2]}/${previous[3]} vs ${contextWindow}/${maxTokens}`,
          );
        }
        entries.set(key, [provider, id, contextWindow, maxTokens]);
      }
    }
  }
  return [...entries.values()].sort(([leftProvider, leftModel], [rightProvider, rightModel]) =>
    leftProvider === rightProvider
      ? leftModel.localeCompare(rightModel)
      : leftProvider.localeCompare(rightProvider),
  );
}

const dataDir = resolveDataDir();
const entries = readEntries(dataDir);
if (entries.length === 0) {
  console.error(`no chat models found in ${dataDir}`);
  process.exit(1);
}
writeFileSync(outputPath, `${JSON.stringify(entries)}\n`);
console.log(
  `wrote ${entries.length} models from ${dataDir} to ${outputPath}`,
);
