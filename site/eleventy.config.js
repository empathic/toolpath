import markdownItAnchor from "markdown-it-anchor";
import markdownIt from "markdown-it";
import Prism from "prismjs";
import loadLanguages from "prismjs/components/index.js";
import { readFileSync } from "fs";

loadLanguages(["json", "bash", "rust"]);

const slugify = (s) =>
  s
    .toLowerCase()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .trim();

function prismHighlight(code, lang) {
  if (lang && Prism.languages[lang]) {
    var highlighted = Prism.highlight(code, Prism.languages[lang], lang);
    return (
      '<pre class="language-' +
      lang +
      '"><code class="language-' +
      lang +
      '">' +
      highlighted +
      "</code></pre>"
    );
  }
  return "";
}

export default function (eleventyConfig) {
  eleventyConfig.addPassthroughCopy("css");
  eleventyConfig.addPassthroughCopy("js");
  eleventyConfig.addPassthroughCopy("wasm");

  // Self-hosted fonts (latin subset only) — pulled from @fontsource packages
  // at install time, copied to /fonts/ at build time. Filenames are stable
  // so <link rel="preload"> and @font-face URLs match.
  eleventyConfig.addPassthroughCopy({
    "node_modules/@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-400-normal.woff2":
      "fonts/plex-mono-400.woff2",
    "node_modules/@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-500-normal.woff2":
      "fonts/plex-mono-500.woff2",
    "node_modules/@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-600-normal.woff2":
      "fonts/plex-mono-600.woff2",
    "node_modules/@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-700-normal.woff2":
      "fonts/plex-mono-700.woff2",
    "node_modules/@fontsource/source-serif-4/files/source-serif-4-latin-400-normal.woff2":
      "fonts/source-serif-400.woff2",
    "node_modules/@fontsource/source-serif-4/files/source-serif-4-latin-400-italic.woff2":
      "fonts/source-serif-400-italic.woff2",
    "node_modules/@fontsource/source-serif-4/files/source-serif-4-latin-600-normal.woff2":
      "fonts/source-serif-600.woff2",
  });

  // Self-hosted vendor JS — no third-party CDNs at runtime.
  eleventyConfig.addPassthroughCopy({
    "node_modules/d3/dist/d3.min.js": "vendor/d3.min.js",
    "node_modules/dagre-d3/dist/dagre-d3.min.js": "vendor/dagre-d3.min.js",
    "node_modules/@xterm/xterm/lib/xterm.js": "vendor/xterm.js",
    "node_modules/@xterm/xterm/css/xterm.css": "vendor/xterm.css",
    "node_modules/@xterm/addon-fit/lib/addon-fit.js":
      "vendor/xterm-addon-fit.js",
    "node_modules/prismjs/components/prism-core.min.js": "vendor/prism.js",
    "node_modules/prismjs/components/prism-json.min.js": "vendor/prism-json.js",
    "node_modules/prismjs/components/prism-diff.min.js": "vendor/prism-diff.js",
  });

  // Brand book is a dev reference, not site content
  eleventyConfig.ignores.add("BRAND.md");

  eleventyConfig.amendLibrary("md", (mdLib) => {
    mdLib.set({ highlight: prismHighlight });
    mdLib.use(markdownItAnchor, {
      permalink: markdownItAnchor.permalink.headerLink(),
      slugify,
    });
  });

  // Load RFC.md from repo root, pre-render to HTML
  const rfcRaw = readFileSync("../RFC.md", "utf-8");
  // Strip the h1 title and the status/authors/created metadata lines,
  // rewrite relative links to point to site pages
  const rfcContent = rfcRaw
    .replace(/^# .+\n+(\*\*.+\n)*/m, "")
    .replace(/\(\.\/FAQ\.md\)/g, "(/faq/)")
    .replace(
      /\(\.\/schema\/toolpath\.schema\.json\)/g,
      "(https://github.com/empathic/toolpath/blob/main/schema/toolpath.schema.json)",
    );
  const rfcMd = markdownIt({
    html: true,
    linkify: true,
    highlight: prismHighlight,
  }).use(markdownItAnchor, {
    permalink: markdownItAnchor.permalink.headerLink(),
    slugify,
  });
  eleventyConfig.addGlobalData("rfcHtml", rfcMd.render(rfcContent));

  // Load example JSON files for the interactive playground
  eleventyConfig.addGlobalData("playgroundFiles", () => {
    const files = [
      "step-01-minimal.json",
      "path-01-pr.path.json",
      "graph-01-release.json",
    ];
    const result = {};
    for (const f of files)
      result[f] = readFileSync(`../examples/${f}`, "utf-8");
    return result;
  });

  // Load all example JSON files for the visualizer dropdown
  eleventyConfig.addGlobalData("vizExamples", () => {
    const examples = [
      { file: "step-01-minimal.json", name: "Step: minimal" },
      { file: "path-01-pr.path.json", name: "Path: PR with dead end" },
      { file: "path-02-local-session.path.json", name: "Path: local session" },
      { file: "path-03-signed-pr.path.json", name: "Path: signed PR" },
      {
        file: "path-04-exploration.path.json",
        name: "Path: exploration & merge",
      },
      { file: "graph-01-release.json", name: "Graph: release bundle" },
    ];
    return examples.map((e) => ({
      name: e.name,
      content: readFileSync(`../examples/${e.file}`, "utf-8"),
    }));
  });

  return {
    dir: {
      input: ".",
      output: "_site",
      includes: "_includes",
      data: "_data",
    },
    markdownTemplateEngine: "njk",
  };
}
