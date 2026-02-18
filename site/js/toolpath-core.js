// Toolpath Core — shared pure-data logic (no DOM dependencies)
// Extracted from visualizer.js for reuse in playground.js

(function () {
  "use strict";

  var TC = {};

  // --- Actor helpers ---
  TC.actorType = function (actor) {
    var colon = actor.indexOf(":");
    return colon > -1 ? actor.substring(0, colon) : actor;
  };

  TC.actorName = function (actor) {
    var colon = actor.indexOf(":");
    return colon > -1 ? actor.substring(colon + 1) : actor;
  };

  TC.resolveActor = function (actorStr, actorDefs) {
    if (!actorDefs) return null;
    return actorDefs[actorStr] || null;
  };

  TC.actorDisplayName = function (actorStr, actorDefs) {
    var def = TC.resolveActor(actorStr, actorDefs);
    if (def && def.name) return def.name;
    return TC.actorName(actorStr);
  };

  TC.actorIdentitySummary = function (actorStr, actorDefs) {
    var def = TC.resolveActor(actorStr, actorDefs);
    if (!def) return "";
    var parts = [];
    if (def.provider) parts.push(def.provider);
    if (def.model) parts.push(def.model);
    if (def.identities) {
      def.identities.forEach(function (id) {
        parts.push(id.system + ":" + id.id);
      });
    }
    return parts.join(", ");
  };

  // --- String helpers ---
  TC.escapeHtml = function (s) {
    if (!s) return "";
    return s
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  };

  TC.truncate = function (s, n) {
    if (!s) return "";
    return s.length > n ? s.substring(0, n) + "..." : s;
  };

  // --- Document parsing ---
  TC.parseDoc = function (text) {
    var doc = JSON.parse(text);
    if (doc.Step) return { type: "Step", data: doc };
    if (doc.Path) return { type: "Path", data: doc };
    if (doc.Graph) return { type: "Graph", data: doc };
    throw new Error(
      "Unknown document type. Expected top-level key: Step, Path, or Graph.",
    );
  };

  // Normalize into array of { pathInfo, steps, headId, base, actors } clusters
  TC.normalizeClusters = function (parsed) {
    var clusters = [];
    if (parsed.type === "Step") {
      var stepMeta = parsed.data.Step.meta || {};
      clusters.push({
        pathInfo: null,
        steps: [parsed.data.Step],
        headId: null,
        base: null,
        actors: stepMeta.actors || null,
      });
    } else if (parsed.type === "Path") {
      var p = parsed.data.Path;
      var pathActors = (p.meta && p.meta.actors) || null;
      clusters.push({
        pathInfo: p.path,
        steps: p.steps,
        headId: p.path.head,
        base: p.path.base || null,
        actors: pathActors,
      });
    } else if (parsed.type === "Graph") {
      var g = parsed.data.Graph;
      var graphActors = (g.meta && g.meta.actors) || null;
      (g.paths || []).forEach(function (entry) {
        if (entry["$ref"]) {
          clusters.push({
            pathInfo: { id: entry["$ref"] },
            steps: [],
            headId: null,
            base: null,
            isRef: true,
            actors: graphActors,
          });
        } else {
          var entryActors = (entry.meta && entry.meta.actors) || graphActors;
          clusters.push({
            pathInfo: entry.path,
            steps: entry.steps || [],
            headId: entry.path.head,
            base: entry.path.base || null,
            actors: entryActors,
          });
        }
      });
    }
    return clusters;
  };

  // --- DAG queries ---

  // Return set (object) of ancestor step IDs reachable from headId
  TC.ancestors = function (steps, headId) {
    var stepMap = {};
    steps.forEach(function (s) {
      stepMap[s.step.id] = s;
    });
    var result = {};
    var stack = [headId];
    while (stack.length > 0) {
      var id = stack.pop();
      if (result[id]) continue;
      result[id] = true;
      var step = stepMap[id];
      if (step && step.step.parents) {
        step.step.parents.forEach(function (p) {
          stack.push(p);
        });
      }
    }
    return result;
  };

  // Return array of steps that are dead ends (not in ancestor set of headId)
  TC.deadEnds = function (steps, headId) {
    if (!headId) return [];
    var ancestorSet = TC.ancestors(steps, headId);
    return steps.filter(function (s) {
      return !ancestorSet[s.step.id];
    });
  };

  // Filter steps whose actor string starts with prefix
  TC.filterByActor = function (steps, prefix) {
    return steps.filter(function (s) {
      return s.step.actor.indexOf(prefix) === 0;
    });
  };

  // Extract {steps, headId, id, meta} from any parsed document type
  TC.extractSteps = function (parsed) {
    if (parsed.type === "Step") {
      return {
        steps: [parsed.data.Step],
        headId: null,
        id: parsed.data.Step.step.id,
        meta: parsed.data.Step.meta || null,
      };
    }
    if (parsed.type === "Path") {
      var p = parsed.data.Path;
      return {
        steps: p.steps,
        headId: p.path.head,
        id: p.path.id,
        meta: p.meta || null,
      };
    }
    if (parsed.type === "Graph") {
      var g = parsed.data.Graph;
      var allSteps = [];
      (g.paths || []).forEach(function (entry) {
        if (!entry["$ref"] && entry.steps) {
          allSteps = allSteps.concat(entry.steps);
        }
      });
      return {
        steps: allSteps,
        headId: null,
        id: g.graph.id,
        meta: g.meta || null,
      };
    }
    return { steps: [], headId: null, id: null, meta: null };
  };

  window.ToolpathCore = TC;
})();
