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
      "path-01-pr.json",
      "graph-01-release.json",
    ];
    const result = {};
    for (const f of files)
      result[f] = readFileSync(`../examples/${f}`, "utf-8");
    return result;
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
