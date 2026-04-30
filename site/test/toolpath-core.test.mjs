// Tests for site/js/toolpath-core.js — the shared pure-data module
// used by the visualizer. Loads every examples/*.json fixture from the
// repo root and exercises parseDoc / normalizeClusters / extractSteps
// so a future format change that breaks the visualizer surfaces here
// instead of in a broken page.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import vm from "node:vm";

const __dirname = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(__dirname, "..", "..");
const exampleDir = join(repoRoot, "examples");

// Load toolpath-core.js into a Node-compatible sandbox. The IIFE expects
// a `window` global; we stub it and read ToolpathCore back off it.
function loadCore() {
  const src = readFileSync(
    join(repoRoot, "site", "js", "toolpath-core.js"),
    "utf-8",
  );
  const sandbox = { window: {} };
  vm.createContext(sandbox);
  vm.runInContext(src, sandbox);
  return sandbox.window.ToolpathCore;
}

const TC = loadCore();
const exampleFiles = readdirSync(exampleDir)
  .filter((f) => f.endsWith(".json"))
  .sort();

test("loads ToolpathCore", () => {
  assert.ok(TC, "ToolpathCore not defined");
  assert.equal(typeof TC.parseDoc, "function");
  assert.equal(typeof TC.normalizeClusters, "function");
  assert.equal(typeof TC.extractSteps, "function");
});

test("there are example fixtures to test against", () => {
  assert.ok(exampleFiles.length > 0, "no example JSON files found");
});

for (const file of exampleFiles) {
  const text = readFileSync(join(exampleDir, file), "utf-8");

  test(`parseDoc accepts ${file}`, () => {
    const parsed = TC.parseDoc(text);
    assert.ok(parsed, "parseDoc returned falsy");
    assert.ok(parsed.type, "parseDoc result missing 'type'");
    assert.ok(parsed.data, "parseDoc result missing 'data'");
  });

  test(`normalizeClusters yields valid clusters for ${file}`, () => {
    const parsed = TC.parseDoc(text);
    const clusters = TC.normalizeClusters(parsed);
    assert.ok(Array.isArray(clusters), "clusters is not an array");
    assert.ok(clusters.length > 0, `no clusters extracted from ${file}`);

    for (const c of clusters) {
      assert.ok(Array.isArray(c.steps), "cluster.steps must be an array");
      if (!c.isRef) {
        for (const s of c.steps) {
          assert.ok(s.step, `step entry missing .step in ${file}`);
          assert.ok(
            typeof s.step.id === "string" && s.step.id.length > 0,
            `step missing step.id in ${file}`,
          );
          assert.ok(
            typeof s.step.actor === "string" && s.step.actor.length > 0,
            `step missing step.actor in ${file}`,
          );
        }
      }
    }
  });

  test(`extractSteps yields valid steps for ${file}`, () => {
    const parsed = TC.parseDoc(text);
    const out = TC.extractSteps(parsed);
    assert.ok(Array.isArray(out.steps), "extractSteps.steps must be an array");
    assert.ok(
      out.steps.length > 0,
      `extractSteps yielded no steps for ${file}`,
    );
    assert.ok(
      typeof out.id === "string" && out.id.length > 0,
      `extractSteps missing .id for ${file}`,
    );
  });
}

test("parseDoc throws on unrecognized shape", () => {
  assert.throws(
    () => TC.parseDoc('{"foo": "bar"}'),
    /unrecognized|unknown|invalid|expected/i,
  );
});

test("normalizeClusters surfaces meta.title as cluster.title", () => {
  // path-01-pr.path.json has meta.title = "Add email validation"
  // (it's the canonical PR-with-dead-end fixture).
  const text = readFileSync(
    join(exampleDir, "path-01-pr.path.json"),
    "utf-8",
  );
  const parsed = TC.parseDoc(text);
  const clusters = TC.normalizeClusters(parsed);
  assert.equal(clusters.length, 1);
  assert.ok(
    typeof clusters[0].title === "string" && clusters[0].title.length > 0,
    "expected cluster.title to be populated from meta.title",
  );
});

test("normalizeClusters yields null cluster.title when meta is absent", () => {
  // step-01-minimal has no path-level meta.title.
  const text = readFileSync(
    join(exampleDir, "step-01-minimal.json"),
    "utf-8",
  );
  const parsed = TC.parseDoc(text);
  const clusters = TC.normalizeClusters(parsed);
  assert.equal(clusters.length, 1);
  assert.equal(clusters[0].title, null);
});

test("ancestors walks parent links from head", () => {
  const text = readFileSync(
    join(exampleDir, "path-04-exploration.path.json"),
    "utf-8",
  );
  const parsed = TC.parseDoc(text);
  const clusters = TC.normalizeClusters(parsed);
  assert.equal(clusters.length, 1);
  const c = clusters[0];
  assert.ok(c.headId, "cluster missing headId");
  const ancestorSet = TC.ancestors(c.steps, c.headId);
  // Head is in its own ancestor set.
  assert.ok(ancestorSet[c.headId], "head not in ancestor set");
  // At least one step must be present in the ancestor set.
  assert.ok(Object.keys(ancestorSet).length >= 1);
});

test("deadEnds returns steps not on the path to head", () => {
  // path-04-exploration has at least one explicit dead end.
  const text = readFileSync(
    join(exampleDir, "path-04-exploration.path.json"),
    "utf-8",
  );
  const parsed = TC.parseDoc(text);
  const clusters = TC.normalizeClusters(parsed);
  const c = clusters[0];
  const dead = TC.deadEnds(c.steps, c.headId);
  assert.ok(Array.isArray(dead));
  // The exploration example has dead ends by construction.
  assert.ok(
    dead.length > 0,
    "expected at least one dead-end step in path-04-exploration",
  );
});
