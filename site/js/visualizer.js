// Toolpath D3 Visualizer
// Uses dagre-d3 for hierarchical DAG layout with the warm brand palette.

(function () {
  "use strict";

  // --- Brand palette ---
  // All colors expressed as CSS expressions so the cascade resolves them
  // per theme. color-mix() handles the per-actor opacity tints.
  var MIX = function (varName, pct) {
    return "color-mix(in srgb, var(" + varName + ") " + pct + "%, transparent)";
  };
  var COLORS = {
    human: { fill: MIX("--accent", 18), stroke: "var(--accent)" },
    agent: { fill: MIX("--accent", 30), stroke: "var(--accent)" },
    tool: {
      fill: MIX("--text-secondary", 15),
      stroke: "var(--text-secondary)",
    },
    ci: { fill: MIX("--text-secondary", 15), stroke: "var(--text-secondary)" },
    dead: { fill: MIX("--alert", 18), stroke: "var(--alert)" },
    base: { fill: "var(--bg-elevated)", stroke: "var(--text-secondary)" },
  };
  var EDGE_ACTIVE = { stroke: "var(--text)", width: 2 };
  var EDGE_INACTIVE = { stroke: "var(--text-secondary)", width: 1 };
  var EDGE_BASE = { stroke: "var(--accent)", width: 1.5 };

  // --- Examples loaded from window.__VIZ_EXAMPLES__ (injected by Eleventy) ---
  var VIZ_EXAMPLES = window.__VIZ_EXAMPLES__ || [];
  // Default to "Path: exploration & merge" (index 4) or first available
  var DEFAULT_EXAMPLE_INDEX = VIZ_EXAMPLES.length > 4 ? 4 : 0;

  // --- DOM refs ---
  var input = document.getElementById("viz-input");
  var highlight = document.getElementById("viz-highlight");
  var fileInput = document.getElementById("viz-file");
  var exampleSelect = document.getElementById("viz-example-select");
  var formatBtn = document.getElementById("viz-format");
  var renderBtn = document.getElementById("viz-render");
  var errorBox = document.getElementById("viz-error");
  var canvas = document.getElementById("viz-canvas");
  var tooltip = document.getElementById("viz-tooltip");
  var detail = document.getElementById("viz-detail");
  var detailTitle = document.getElementById("viz-detail-title");
  var detailBody = document.getElementById("viz-detail-body");
  var detailClose = document.getElementById("viz-detail-close");
  var detailResize = document.getElementById("viz-detail-resize");
  var zoomInBtn = document.getElementById("viz-zoom-in");
  var zoomOutBtn = document.getElementById("viz-zoom-out");
  var fitBtn = document.getElementById("viz-fit");
  var showDead = document.getElementById("viz-show-dead");
  var showTs = document.getElementById("viz-show-ts");
  var showFiles = document.getElementById("viz-show-files");

  // --- State ---
  var currentDoc = null;
  var zoomBehavior = null;
  var svgGroup = null;

  // --- Helpers (delegated to ToolpathCore) ---
  var TC = window.ToolpathCore;

  function actorType(actor) {
    return TC.actorType(actor);
  }
  function actorColors(actor) {
    var t = actorType(actor);
    return COLORS[t] || COLORS.tool;
  }
  function actorDisplayName(actorStr, actorDefs) {
    return TC.actorDisplayName(actorStr, actorDefs);
  }
  function actorIdentitySummary(actorStr, actorDefs) {
    return TC.actorIdentitySummary(actorStr, actorDefs);
  }
  function resolveActor(actorStr, actorDefs) {
    return TC.resolveActor(actorStr, actorDefs);
  }
  function truncate(s, n) {
    return TC.truncate(s, n);
  }
  function escapeHtml(s) {
    return TC.escapeHtml(s);
  }
  function ancestors(steps, headId) {
    return TC.ancestors(steps, headId);
  }
  function parseDoc(text) {
    return TC.parseDoc(text);
  }
  function normalizeClusters(parsed) {
    return TC.normalizeClusters(parsed);
  }

  // --- Render graph ---
  function render() {
    hideError();
    hideDetail();

    var text = input.value.trim();
    if (!text) {
      showError("Paste or load a Toolpath JSON document first.");
      return;
    }

    var parsed;
    try {
      parsed = parseDoc(text);
    } catch (e) {
      showError("Parse error: " + e.message);
      return;
    }

    currentDoc = parsed;
    var clusters = normalizeClusters(parsed);
    if (clusters.length === 0) {
      showError("No paths or steps found in document.");
      return;
    }

    drawGraph(clusters);
  }

  function drawGraph(clusters) {
    // Clear previous
    d3.select(canvas).selectAll("*").remove();

    var g = new dagreD3.graphlib.Graph({ compound: true, multigraph: false })
      .setGraph({
        rankdir: "TB",
        nodesep: 60,
        ranksep: 50,
        marginx: 30,
        marginy: 30,
      })
      .setDefaultEdgeLabel(function () {
        return {};
      });

    var deadSets = {}; // cluster index → ancestors set
    var showDeadEnds = showDead.checked;
    var showTimestamps = showTs.checked;
    var showFilesList = showFiles.checked;

    // Detect bases shared across clusters so we can render a single BASE node
    // for paths that branched from the same commit. The shared node sits at
    // the graph level (no cluster parent) so dagre lays it out above the
    // clusters that point into it.
    function baseKey(b) {
      if (!b) return null;
      return [b.uri || "", b.ref || "", b.branch || ""].join("|");
    }
    var baseCounts = {};
    clusters.forEach(function (c) {
      var k = baseKey(c.base);
      if (k) baseCounts[k] = (baseCounts[k] || 0) + 1;
    });
    var sharedBaseNodeFor = {}; // key → nodeId (emitted on first encounter)
    var clusterBaseNodeId = {}; // cluster index → nodeId for edges below

    clusters.forEach(function (cluster, ci) {
      var prefix = clusters.length > 1 ? "c" + ci + "/" : "";
      var ancestorSet = cluster.headId
        ? ancestors(cluster.steps, cluster.headId)
        : null;
      deadSets[ci] = ancestorSet;

      // For $ref clusters: skip the cluster border and emit a single
      // standalone placeholder. The cluster border + detached node
      // combination read as broken.
      if (cluster.isRef) {
        var refId = "ref_" + ci;
        g.setNode(refId, {
          label: "$ref → " + truncate(cluster.pathInfo.id, 40),
          shape: "rect",
          style:
            "fill: " +
            MIX("--text-secondary", 15) +
            "; stroke: var(--text-secondary); stroke-dasharray: 4,3; stroke-width: 1px;",
          labelStyle:
            "font-family: 'IBM Plex Mono', monospace; font-size: 10px; font-style: italic;",
        });
        return;
      }

      if (clusters.length > 1) {
        var clusterId = "cluster_" + ci;
        g.setNode(clusterId, {
          label:
            cluster.title ||
            (cluster.pathInfo && cluster.pathInfo.id) ||
            "cluster-" + ci,
          clusterLabelPos: "top",
          style:
            "fill: transparent; stroke: " +
            MIX("--accent", 15) +
            "; stroke-dasharray: 4,3;",
        });
      }

      // BASE node — shared across clusters when their base.uri+ref+branch
      // match, otherwise per-cluster. Shared BASEs sit at graph level so
      // multiple clusters can point into them.
      if (cluster.base) {
        var bk = baseKey(cluster.base);
        var isShared = baseCounts[bk] > 1;
        var baseNodeId;
        if (isShared) {
          if (!sharedBaseNodeFor[bk]) {
            baseNodeId = "shared_base_" + Object.keys(sharedBaseNodeFor).length;
            sharedBaseNodeFor[bk] = baseNodeId;
            g.setNode(baseNodeId, {
              label: "BASE",
              shape: "ellipse",
              style:
                "fill: " +
                COLORS.base.fill +
                "; stroke: " +
                COLORS.base.stroke +
                "; stroke-width: 2px;",
              labelStyle:
                "font-family: 'IBM Plex Mono', monospace; font-size: 10px; font-weight: 600;",
            });
            // No setParent → graph-level so it can feed multiple clusters.
          } else {
            baseNodeId = sharedBaseNodeFor[bk];
          }
        } else {
          baseNodeId = prefix + "__BASE__";
          g.setNode(baseNodeId, {
            label: "BASE",
            shape: "ellipse",
            style:
              "fill: " +
              COLORS.base.fill +
              "; stroke: " +
              COLORS.base.stroke +
              "; stroke-width: 2px;",
            labelStyle:
              "font-family: 'IBM Plex Mono', monospace; font-size: 10px; font-weight: 600;",
          });
          if (clusters.length > 1) {
            g.setParent(baseNodeId, "cluster_" + ci);
          }
        }
        clusterBaseNodeId[ci] = baseNodeId;
      }

      // Find root steps (no parents)
      var rootSteps = [];

      cluster.steps.forEach(function (s) {
        var sid = s.step.id;
        var nodeId = prefix + sid;
        var isDead = ancestorSet && !ancestorSet[sid];
        var isHead = sid === cluster.headId;
        var colors = actorColors(s.step.actor);

        if (!s.step.parents || s.step.parents.length === 0) {
          rootSteps.push(nodeId);
        }

        // Skip dead-end nodes when toggle is off
        if (isDead && !showDeadEnds) return;

        // Build label
        var labelLines = [];
        labelLines.push(sid);
        labelLines.push(actorDisplayName(s.step.actor, cluster.actors));
        if (s.meta && s.meta.intent) {
          labelLines.push(truncate(s.meta.intent, 30));
        }
        if (showTimestamps && s.step.timestamp) {
          labelLines.push(s.step.timestamp.substring(11, 19));
        }
        if (showFilesList && s.change) {
          var files = Object.keys(s.change);
          files.forEach(function (f) {
            labelLines.push(truncate(f, 28));
          });
        }

        var fill = isDead ? COLORS.dead.fill : colors.fill;
        var stroke = isDead ? COLORS.dead.stroke : colors.stroke;
        var strokeWidth = isHead ? "3px" : "1.5px";
        var dashArray = isDead
          ? "4,3"
          : actorType(s.step.actor) === "ci"
            ? "4,3"
            : "none";
        var fontWeight = isHead ? "font-weight: bold;" : "";

        g.setNode(nodeId, {
          label: labelLines.join("\n"),
          shape: "rect",
          style:
            "fill: " +
            fill +
            "; stroke: " +
            stroke +
            "; stroke-width: " +
            strokeWidth +
            "; stroke-dasharray: " +
            dashArray +
            ";",
          labelStyle:
            "font-family: 'IBM Plex Mono', monospace; font-size: 10px; " +
            fontWeight,
          _stepData: s,
          _isDead: isDead,
          _isHead: isHead,
          _clusterId: ci,
        });

        if (clusters.length > 1) {
          g.setParent(nodeId, "cluster_" + ci);
        }
      });

      // Add edges
      cluster.steps.forEach(function (s) {
        var sid = s.step.id;
        var targetId = prefix + sid;
        var isDead = ancestorSet && !ancestorSet[sid];

        if (isDead && !showDeadEnds) return;

        if (s.step.parents) {
          s.step.parents.forEach(function (pid) {
            var sourceId = prefix + pid;
            // Don't add edge if parent node doesn't exist in graph
            // (e.g. standalone Step docs referencing external parents)
            if (!g.node(sourceId)) return;
            // Don't add edge if parent is hidden dead-end
            if (!showDeadEnds && ancestorSet && !ancestorSet[pid]) return;

            var bothActive =
              ancestorSet && ancestorSet[sid] && ancestorSet[pid];
            var edgeStyle = bothActive ? EDGE_ACTIVE : EDGE_INACTIVE;
            var dash = bothActive ? "" : "4,3";

            g.setEdge(sourceId, targetId, {
              style:
                "stroke: " +
                edgeStyle.stroke +
                "; stroke-width: " +
                edgeStyle.width +
                "px;" +
                (dash ? " stroke-dasharray: " + dash + ";" : ""),
              arrowheadStyle: "fill: " + edgeStyle.stroke,
              curve: d3.curveBasis,
            });
          });
        }
      });

      // Connect BASE to root steps
      if (cluster.base) {
        var baseNodeId = clusterBaseNodeId[ci];
        rootSteps.forEach(function (rootId) {
          // Only connect if the root node exists in the graph
          if (g.node(rootId)) {
            g.setEdge(baseNodeId, rootId, {
              style:
                "stroke: " +
                EDGE_BASE.stroke +
                "; stroke-width: " +
                EDGE_BASE.width +
                "px;",
              arrowheadStyle: "fill: " + EDGE_BASE.stroke,
              curve: d3.curveBasis,
            });
          }
        });
      }
    });

    // Render with dagre-d3
    var svg = d3.select(canvas);
    svgGroup = svg.append("g");

    var dagreRender = new dagreD3.render();
    dagreRender(svgGroup, g);

    // Setup zoom
    zoomBehavior = d3
      .zoom()
      .scaleExtent([0.1, 4])
      .on("zoom", function (event) {
        svgGroup.attr("transform", event.transform);
      });
    svg.call(zoomBehavior);

    // Fit to view
    fitToView();

    // Interactions
    setupInteractions(g, clusters, deadSets);
  }

  function fitToView() {
    if (!svgGroup || !zoomBehavior) return;
    var svg = d3.select(canvas);
    var bounds = svgGroup.node().getBBox();
    if (bounds.width === 0 || bounds.height === 0) return;

    var parent = canvas.parentElement;
    var fullWidth = parent.clientWidth;
    var fullHeight = parent.clientHeight;
    var scale = Math.min(
      fullWidth / (bounds.width + 60),
      fullHeight / (bounds.height + 60),
      1.5,
    );
    var tx = (fullWidth - bounds.width * scale) / 2 - bounds.x * scale;
    var ty = (fullHeight - bounds.height * scale) / 2 - bounds.y * scale;

    svg
      .transition()
      .duration(400)
      .call(
        zoomBehavior.transform,
        d3.zoomIdentity.translate(tx, ty).scale(scale),
      );
  }

  function setupInteractions(g, clusters, deadSets) {
    var nodes = d3.select(canvas).selectAll("g.node");

    // Hover tooltip
    nodes.on("mouseenter", function (event) {
      var nodeId = d3.select(this).attr("id") || "";
      // dagre-d3 stores node id in the data attribute
      var data = this.__data__;
      var nodeData = g.node(data);
      if (!nodeData || !nodeData._stepData) return;

      var s = nodeData._stepData;
      var isDead = nodeData._isDead;
      var isHead = nodeData._isHead;
      var clusterActors = clusters[nodeData._clusterId]
        ? clusters[nodeData._clusterId].actors
        : null;

      var html = [];
      html.push("<div><strong>" + escapeHtml(s.step.id) + "</strong>");
      if (isHead) html.push(' <span style="color:var(--accent)">(HEAD)</span>');
      if (isDead) html.push(' <span class="tt-dead">(dead end)</span>');
      html.push("</div>");
      var displayName = actorDisplayName(s.step.actor, clusterActors);
      html.push(
        '<div class="tt-label">Actor</div><div>' +
          escapeHtml(displayName) +
          ' <span style="color:var(--text-secondary)">' +
          escapeHtml(s.step.actor) +
          "</span></div>",
      );
      var idSummary = actorIdentitySummary(s.step.actor, clusterActors);
      if (idSummary) {
        html.push(
          '<div style="color:var(--text-secondary);font-size:0.68rem">' +
            escapeHtml(idSummary) +
            "</div>",
        );
      }
      if (s.step.timestamp) {
        html.push(
          '<div class="tt-label">Timestamp</div><div>' +
            escapeHtml(s.step.timestamp) +
            "</div>",
        );
      }
      if (s.meta && s.meta.intent) {
        html.push(
          '<div class="tt-label">Intent</div><div class="tt-intent">' +
            escapeHtml(s.meta.intent) +
            "</div>",
        );
      }
      if (s.change) {
        var files = Object.keys(s.change);
        html.push(
          '<div class="tt-label">Artifacts (' + files.length + ")</div>",
        );
        files.forEach(function (f) {
          html.push("<div>" + escapeHtml(f) + "</div>");
        });
      }

      tooltip.innerHTML = html.join("");
      tooltip.hidden = false;
    });

    nodes.on("mousemove", function (event) {
      var wrap = canvas.parentElement;
      var rect = wrap.getBoundingClientRect();
      var x = event.clientX - rect.left + 12;
      var y = event.clientY - rect.top + 12;
      // Keep tooltip in bounds
      if (x + tooltip.offsetWidth > rect.width)
        x = x - tooltip.offsetWidth - 24;
      if (y + tooltip.offsetHeight > rect.height)
        y = y - tooltip.offsetHeight - 24;
      tooltip.style.left = x + "px";
      tooltip.style.top = y + "px";
    });

    nodes.on("mouseleave", function () {
      tooltip.hidden = true;
    });

    // Click detail panel
    nodes.on("click", function (event) {
      var data = this.__data__;
      var nodeData = g.node(data);
      if (!nodeData || !nodeData._stepData) return;

      event.stopPropagation();
      showDetail(
        nodeData._stepData,
        nodeData._isHead,
        nodeData._isDead,
        g,
        clusters,
      );
    });

    // Click canvas to close detail
    d3.select(canvas).on("click", function () {
      hideDetail();
    });
  }

  function showDetail(step, isHead, isDead, g, clusters) {
    detailTitle.textContent =
      step.step.id + (isHead ? " (HEAD)" : "") + (isDead ? " (dead end)" : "");
    detail.hidden = false;

    // Find actor definitions for this step's cluster
    var stepActorDefs = null;
    for (var ci = 0; ci < clusters.length; ci++) {
      for (var si = 0; si < clusters[ci].steps.length; si++) {
        if (clusters[ci].steps[si].step.id === step.step.id) {
          stepActorDefs = clusters[ci].actors;
          break;
        }
      }
      if (stepActorDefs !== null) break;
    }

    var html = [];

    // Actor
    var actorDef = resolveActor(step.step.actor, stepActorDefs);
    html.push('<div class="detail-section">');
    html.push('<div class="detail-label">Actor</div>');
    html.push("<div>" + escapeHtml(step.step.actor) + "</div>");
    if (actorDef) {
      if (actorDef.name) {
        html.push(
          "<div><strong>" + escapeHtml(actorDef.name) + "</strong></div>",
        );
      }
      if (actorDef.provider || actorDef.model) {
        var providerParts = [];
        if (actorDef.provider) providerParts.push(actorDef.provider);
        if (actorDef.model) providerParts.push(actorDef.model);
        html.push(
          '<div style="color:var(--text-secondary)">' +
            escapeHtml(providerParts.join(" / ")) +
            "</div>",
        );
      }
      if (actorDef.identities && actorDef.identities.length > 0) {
        actorDef.identities.forEach(function (id) {
          html.push(
            '<div style="color:var(--text-secondary)">' +
              escapeHtml(id.system) +
              ": " +
              escapeHtml(id.id) +
              "</div>",
          );
        });
      }
    }
    html.push("</div>");

    // Timestamp
    if (step.step.timestamp) {
      html.push('<div class="detail-section">');
      html.push('<div class="detail-label">Timestamp</div>');
      html.push("<div>" + escapeHtml(step.step.timestamp) + "</div>");
      html.push("</div>");
    }

    // Intent
    if (step.meta && step.meta.intent) {
      html.push('<div class="detail-section">');
      html.push('<div class="detail-label">Intent</div>');
      html.push(
        '<div style="font-style:italic">' +
          escapeHtml(step.meta.intent) +
          "</div>",
      );
      html.push("</div>");
    }

    // Parents
    if (step.step.parents && step.step.parents.length > 0) {
      html.push('<div class="detail-section">');
      html.push('<div class="detail-label">Parents</div>');
      html.push('<div class="detail-nav">');
      step.step.parents.forEach(function (pid) {
        html.push(
          '<a data-nav-step="' +
            escapeHtml(pid) +
            '">' +
            escapeHtml(pid) +
            "</a>",
        );
      });
      html.push("</div></div>");
    }

    // Children (find steps that reference this one as parent)
    var children = findChildren(step.step.id, clusters);
    if (children.length > 0) {
      html.push('<div class="detail-section">');
      html.push('<div class="detail-label">Children</div>');
      html.push('<div class="detail-nav">');
      children.forEach(function (cid) {
        html.push(
          '<a data-nav-step="' +
            escapeHtml(cid) +
            '">' +
            escapeHtml(cid) +
            "</a>",
        );
      });
      html.push("</div></div>");
    }

    // Artifacts + diffs
    if (step.change) {
      var files = Object.keys(step.change);
      html.push('<div class="detail-section">');
      html.push(
        '<div class="detail-label">Changes (' + files.length + ")</div>",
      );
      files.forEach(function (f) {
        html.push(
          '<div style="margin-top:0.4rem"><strong>' +
            escapeHtml(f) +
            "</strong></div>",
        );
        var ch = step.change[f];
        if (ch.raw) {
          html.push(
            "<pre>" +
              Prism.highlight(ch.raw, Prism.languages.diff, "diff") +
              "</pre>",
          );
        }
        if (ch.structural) {
          html.push(
            "<pre>" +
              Prism.highlight(
                JSON.stringify(ch.structural, null, 2),
                Prism.languages.json,
                "json",
              ) +
              "</pre>",
          );
        }
      });
      html.push("</div>");
    }

    // Full JSON
    html.push('<div class="detail-section">');
    html.push('<div class="detail-label">Raw JSON</div>');
    html.push(
      "<pre>" +
        Prism.highlight(
          JSON.stringify(step, null, 2),
          Prism.languages.json,
          "json",
        ) +
        "</pre>",
    );
    html.push("</div>");

    detailBody.innerHTML = html.join("");

    // Wire up nav links
    detailBody.querySelectorAll("[data-nav-step]").forEach(function (link) {
      link.addEventListener("click", function (e) {
        e.preventDefault();
        var targetId = this.getAttribute("data-nav-step");
        var targetStep = findStep(targetId, clusters);
        if (targetStep) {
          var headId = findHeadForStep(targetId, clusters);
          var ancestorSet = headId
            ? ancestors(getAllSteps(clusters), headId)
            : null;
          var isTargetDead = ancestorSet ? !ancestorSet[targetId] : false;
          var isTargetHead = targetId === headId;
          showDetail(targetStep, isTargetHead, isTargetDead, g, clusters);
        }
      });
    });
  }

  function findChildren(stepId, clusters) {
    var children = [];
    clusters.forEach(function (cluster) {
      cluster.steps.forEach(function (s) {
        if (s.step.parents && s.step.parents.indexOf(stepId) > -1) {
          children.push(s.step.id);
        }
      });
    });
    return children;
  }

  function findStep(stepId, clusters) {
    for (var i = 0; i < clusters.length; i++) {
      for (var j = 0; j < clusters[i].steps.length; j++) {
        if (clusters[i].steps[j].step.id === stepId) {
          return clusters[i].steps[j];
        }
      }
    }
    return null;
  }

  function findHeadForStep(stepId, clusters) {
    for (var i = 0; i < clusters.length; i++) {
      for (var j = 0; j < clusters[i].steps.length; j++) {
        if (clusters[i].steps[j].step.id === stepId) {
          return clusters[i].headId;
        }
      }
    }
    return null;
  }

  function getAllSteps(clusters) {
    var all = [];
    clusters.forEach(function (c) {
      all = all.concat(c.steps);
    });
    return all;
  }

  function hideDetail() {
    detail.hidden = true;
  }

  function showError(msg) {
    errorBox.textContent = msg;
    errorBox.hidden = false;
  }

  function hideError() {
    errorBox.hidden = true;
  }

  // --- Input highlight sync ---
  function syncHighlight() {
    var code = input.value;
    highlight.innerHTML = code
      ? Prism.highlight(code, Prism.languages.json, "json")
      : "";
    highlight.scrollTop = input.scrollTop;
  }

  // --- Format toggle ---
  // Button label reflects the *current* state of the textarea: "Pretty" when
  // the text has internal newlines, "Compact" otherwise. We trim before
  // sniffing so trailing newlines (common on files saved by editors) don't
  // mislabel compact docs as pretty. Clicking flips to the other format.
  function updateFormatBtnLabel() {
    var hasNewline = input.value.trim().indexOf("\n") !== -1;
    formatBtn.textContent = hasNewline ? "Pretty" : "Compact";
  }

  // Push the toggle left by the textarea's scrollbar width so it doesn't
  // overlap the scrollbar. offsetWidth - clientWidth reads as 0 on systems
  // with overlay scrollbars (default macOS) and ~15-17px with classic
  // scrollbars (Windows, Linux, macOS with "Always show"). Re-measure when
  // content changes, since the scrollbar appears/disappears with overflow.
  function updateScrollbarOffset() {
    var w = Math.max(0, input.offsetWidth - input.clientWidth);
    input.parentElement.style.setProperty("--viz-scrollbar-w", w + "px");
  }

  formatBtn.addEventListener("click", function () {
    var text = input.value.trim();
    if (!text) return;
    var parsed;
    try {
      parsed = JSON.parse(text);
    } catch (e) {
      showError("Can't reformat — JSON is invalid: " + e.message);
      return;
    }
    hideError();
    var isPretty = input.value.trim().indexOf("\n") !== -1;
    input.value = isPretty
      ? JSON.stringify(parsed)
      : JSON.stringify(parsed, null, 2);
    syncHighlight();
    updateFormatBtnLabel();
    updateScrollbarOffset();
  });

  input.addEventListener("input", function () {
    syncHighlight();
    updateFormatBtnLabel();
    updateScrollbarOffset();
  });
  input.addEventListener("scroll", function () {
    highlight.scrollTop = input.scrollTop;
  });
  window.addEventListener("resize", updateScrollbarOffset);

  // --- Editor vertical resize ---
  // Custom drag handle replaces the native textarea grip. Height is applied
  // directly to the textarea (the editor container auto-fits) and persisted
  // to localStorage so the size sticks across reloads.
  var editorResize = document.getElementById("viz-editor-resize");
  var EDITOR_HEIGHT_KEY = "toolpath:viz:editor-height";
  var EDITOR_HEIGHT_MIN = 80;
  function applyEditorHeight(h) {
    input.style.height = h + "px";
    updateScrollbarOffset();
  }
  try {
    var savedH = parseInt(localStorage.getItem(EDITOR_HEIGHT_KEY) || "", 10);
    if (Number.isFinite(savedH) && savedH >= EDITOR_HEIGHT_MIN) {
      applyEditorHeight(savedH);
    }
  } catch (e) {
    // localStorage may be blocked; fall back to default rows="6" sizing.
  }

  if (editorResize) {
    var editorResizing = false;
    var startY = 0;
    var startH = 0;
    editorResize.addEventListener("pointerdown", function (e) {
      e.preventDefault();
      editorResizing = true;
      startY = e.clientY;
      startH = input.getBoundingClientRect().height;
      document.body.dataset.vizEditorResizing = "1";
      try {
        editorResize.setPointerCapture(e.pointerId);
      } catch (err) {
        // Some browsers refuse pointer capture on synthetic events; OK.
      }
    });
    editorResize.addEventListener("pointermove", function (e) {
      if (!editorResizing) return;
      var newH = Math.max(EDITOR_HEIGHT_MIN, startH + (e.clientY - startY));
      applyEditorHeight(newH);
    });
    function endEditorResize(e) {
      if (!editorResizing) return;
      editorResizing = false;
      delete document.body.dataset.vizEditorResizing;
      try {
        editorResize.releasePointerCapture(e.pointerId);
      } catch (err) {
        // Capture may already be released.
      }
      try {
        var h = parseInt(input.style.height, 10);
        if (Number.isFinite(h)) {
          localStorage.setItem(EDITOR_HEIGHT_KEY, String(h));
        }
      } catch (err) {
        // localStorage blocked — height persists for this session only.
      }
    }
    editorResize.addEventListener("pointerup", endEditorResize);
    editorResize.addEventListener("pointercancel", endEditorResize);
  }

  // --- Event wiring ---
  renderBtn.addEventListener("click", render);

  exampleSelect.addEventListener("change", function () {
    var idx = exampleSelect.value;
    if (idx === "" || !VIZ_EXAMPLES[idx]) return;
    var content = VIZ_EXAMPLES[idx].content;
    // Pretty-print if it parses as JSON
    try {
      content = JSON.stringify(JSON.parse(content), null, 2);
    } catch (e) {}
    input.value = content;
    syncHighlight();
    updateFormatBtnLabel();
    updateScrollbarOffset();
    render();
  });

  function loadFile(file) {
    if (!file) return;
    // No extension/MIME gate — Toolpath docs ship as .json, .path, .path.json,
    // and .path.jsonl, plus the picker has always accepted anything. Validate
    // JSON before clobbering the textarea so a stray binary or text file
    // doesn't replace the user's current document with garbage.
    var reader = new FileReader();
    reader.onload = function () {
      var text = reader.result;
      try {
        JSON.parse(text);
      } catch (err) {
        var label = file.name ? " (" + file.name + ")" : "";
        showError("Not valid JSON" + label + ": " + err.message);
        return;
      }
      hideError();
      input.value = text;
      syncHighlight();
      updateFormatBtnLabel();
      updateScrollbarOffset();
      render();
    };
    reader.onerror = function () {
      showError("Failed to read file: " + (file.name || ""));
    };
    reader.readAsText(file);
  }

  fileInput.addEventListener("change", function () {
    loadFile(fileInput.files[0]);
    // Reset so re-selecting the same file fires `change` again.
    fileInput.value = "";
  });

  // --- Drag and drop ---
  // Drop zone covers the whole visualizer layout. We only react to drags that
  // carry files, and use an enter/leave counter because dragenter/dragleave
  // also fire when the cursor crosses child element boundaries.
  var dropZone = document.querySelector(".viz-layout");
  if (dropZone) {
    var dragDepth = 0;
    function dragHasFiles(e) {
      var types = e.dataTransfer && e.dataTransfer.types;
      if (!types) return false;
      for (var i = 0; i < types.length; i++) {
        if (types[i] === "Files") return true;
      }
      return false;
    }
    dropZone.addEventListener("dragenter", function (e) {
      if (!dragHasFiles(e)) return;
      e.preventDefault();
      dragDepth++;
      dropZone.classList.add("viz-drag-active");
    });
    dropZone.addEventListener("dragover", function (e) {
      if (!dragHasFiles(e)) return;
      e.preventDefault();
      e.dataTransfer.dropEffect = "copy";
    });
    dropZone.addEventListener("dragleave", function (e) {
      if (!dragHasFiles(e)) return;
      dragDepth = Math.max(0, dragDepth - 1);
      if (dragDepth === 0) dropZone.classList.remove("viz-drag-active");
    });
    dropZone.addEventListener("drop", function (e) {
      if (!dragHasFiles(e)) return;
      e.preventDefault();
      dragDepth = 0;
      dropZone.classList.remove("viz-drag-active");
      var files = e.dataTransfer.files;
      if (files && files.length > 0) loadFile(files[0]);
    });
  }

  detailClose.addEventListener("click", hideDetail);

  zoomInBtn.addEventListener("click", function () {
    d3.select(canvas)
      .transition()
      .duration(200)
      .call(zoomBehavior.scaleBy, 1.3);
  });

  zoomOutBtn.addEventListener("click", function () {
    d3.select(canvas)
      .transition()
      .duration(200)
      .call(zoomBehavior.scaleBy, 0.7);
  });

  fitBtn.addEventListener("click", fitToView);

  // Toggle controls trigger re-render
  showDead.addEventListener("change", function () {
    if (currentDoc) render();
  });
  showTs.addEventListener("change", function () {
    if (currentDoc) render();
  });
  showFiles.addEventListener("change", function () {
    if (currentDoc) render();
  });

  // Keyboard shortcut: Escape closes detail
  document.addEventListener("keydown", function (e) {
    if (e.key === "Escape") hideDetail();
  });

  // --- Detail panel resize ---
  // Drag the left edge to resize the right drawer. Width is persisted in
  // localStorage and applied via the --viz-detail-width CSS variable so the
  // mobile @media rule (width: 100vw under 640px) cleanly wins.
  var DETAIL_WIDTH_STORAGE_KEY = "toolpath:viz:detail-width";
  var DETAIL_WIDTH_MIN = 280;
  function detailWidthMax() {
    return Math.min(900, Math.floor(window.innerWidth * 0.9));
  }
  function clampDetailWidth(w) {
    return Math.max(DETAIL_WIDTH_MIN, Math.min(detailWidthMax(), w));
  }
  function applyDetailWidth(w) {
    detail.style.setProperty("--viz-detail-width", w + "px");
  }

  var currentDetailWidth = 380;
  try {
    var saved = parseInt(
      localStorage.getItem(DETAIL_WIDTH_STORAGE_KEY) || "",
      10,
    );
    if (Number.isFinite(saved) && saved > 0) {
      currentDetailWidth = clampDetailWidth(saved);
      applyDetailWidth(currentDetailWidth);
    }
  } catch (e) {
    // localStorage may be blocked; fall back to default width.
  }

  var resizing = false;
  var resizeRaf = 0;

  detailResize.addEventListener("pointerdown", function (e) {
    e.preventDefault();
    resizing = true;
    document.body.dataset.vizResizing = "1";
    detailResize.setPointerCapture(e.pointerId);
  });

  detailResize.addEventListener("pointermove", function (e) {
    if (!resizing) return;
    // Drawer is anchored to right: 0, so width = innerWidth - clientX.
    var w = clampDetailWidth(window.innerWidth - e.clientX);
    if (w === currentDetailWidth) return;
    currentDetailWidth = w;
    if (!resizeRaf) {
      resizeRaf = requestAnimationFrame(function () {
        resizeRaf = 0;
        applyDetailWidth(currentDetailWidth);
      });
    }
  });

  function endResize(e) {
    if (!resizing) return;
    resizing = false;
    delete document.body.dataset.vizResizing;
    try {
      detailResize.releasePointerCapture(e.pointerId);
    } catch (err) {
      // Safe to ignore — capture may already be released.
    }
    if (resizeRaf) {
      cancelAnimationFrame(resizeRaf);
      resizeRaf = 0;
      applyDetailWidth(currentDetailWidth);
    }
    try {
      localStorage.setItem(
        DETAIL_WIDTH_STORAGE_KEY,
        String(currentDetailWidth),
      );
    } catch (err) {
      // localStorage blocked — width stays for this session only.
    }
  }
  detailResize.addEventListener("pointerup", endResize);
  detailResize.addEventListener("pointercancel", endResize);

  window.addEventListener("resize", function () {
    var clamped = clampDetailWidth(currentDetailWidth);
    if (clamped !== currentDetailWidth) {
      currentDetailWidth = clamped;
      applyDetailWidth(currentDetailWidth);
    }
  });

  // --- Auto-load example on page load ---
  if (VIZ_EXAMPLES.length > 0) {
    var content = VIZ_EXAMPLES[DEFAULT_EXAMPLE_INDEX].content;
    try {
      content = JSON.stringify(JSON.parse(content), null, 2);
    } catch (e) {}
    input.value = content;
    exampleSelect.value = String(DEFAULT_EXAMPLE_INDEX);
    syncHighlight();
    updateFormatBtnLabel();
    render();
  } else {
    updateFormatBtnLabel();
  }
  updateScrollbarOffset();
})();
