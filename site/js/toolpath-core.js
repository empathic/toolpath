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
  // Toolpath documents are a single Graph at the root:
  //   { graph: {...}, paths: [...] }
  // What used to be a bare Step or Path document is now a single-path Graph.
  TC.parseDoc = function (text) {
    var doc = JSON.parse(text);
    if (
      doc &&
      typeof doc === "object" &&
      doc.graph &&
      Array.isArray(doc.paths)
    ) {
      return { type: "Graph", data: doc };
    }
    throw new Error(
      "Unrecognized document shape. Expected a Graph with top-level 'graph' and 'paths'.",
    );
  };

  // Normalize into array of { pathInfo, steps, headId, base, actors } clusters
  TC.normalizeClusters = function (parsed) {
    var clusters = [];
    var g = parsed.data;
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
          title: null,
        });
      } else {
        var entryActors = (entry.meta && entry.meta.actors) || graphActors;
        var entryTitle = (entry.meta && entry.meta.title) || null;
        clusters.push({
          pathInfo: entry.path,
          steps: entry.steps || [],
          headId: entry.path && entry.path.head ? entry.path.head : null,
          base: (entry.path && entry.path.base) || null,
          actors: entryActors,
          title: entryTitle,
        });
      }
    });
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

  // Extract {steps, headId, id, meta} from a parsed Graph document.
  // headId is set only for single-path graphs (the natural focal point);
  // multi-path graphs leave headId null and let the caller pick per-path.
  TC.extractSteps = function (parsed) {
    var g = parsed.data;
    var paths = g.paths || [];
    var allSteps = [];
    paths.forEach(function (entry) {
      if (!entry["$ref"] && entry.steps) {
        allSteps = allSteps.concat(entry.steps);
      }
    });
    var headId = null;
    if (paths.length === 1 && !paths[0]["$ref"] && paths[0].path) {
      headId = paths[0].path.head || null;
    }
    return {
      steps: allSteps,
      headId: headId,
      id: g.graph ? g.graph.id : null,
      meta: g.meta || null,
    };
  };

  window.ToolpathCore = TC;
})();
