import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { load } from "cheerio";
import { redirects } from "../redirects.mjs";
import { docsRoot, origin, outputRoot } from "./content.mjs";

const html = new Map();
function collect(directory, prefix = "/") {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (entry.isDirectory())
      collect(resolve(directory, entry.name), `${prefix}${entry.name}/`);
    else if (entry.name.endsWith(".html"))
      html.set(
        prefix + entry.name,
        load(readFileSync(resolve(directory, entry.name), "utf8")),
      );
  }
}
collect(outputRoot);
const legacy = JSON.parse(
  readFileSync(resolve(docsRoot, "tests/legacy-routes.json"), "utf8"),
);
for (const [path, anchors] of Object.entries(legacy)) {
  const $ = html.get(path);
  assert.ok($, `Missing legacy page: ${path}`);
  const ids = new Set(
    $("[id]")
      .toArray()
      .map((node) => $(node).attr("id")),
  );
  for (const anchor of anchors)
    assert.ok(ids.has(anchor), `Missing legacy anchor: ${path}#${anchor}`);
}
const sitemap = readFileSync(resolve(outputRoot, "sitemap.xml"), "utf8");
for (const [path, $] of html) {
  if (path === "/404.html") continue;
  const canonical = origin + (path === "/index.html" ? "/" : path);
  assert.equal($("link[rel=canonical]").attr("href"), canonical);
  assert.equal($("meta[property='og:url']").attr("content"), canonical);
  assert.ok($("meta[property='og:title']").attr("content"));
  assert.ok(
    sitemap.includes(`<loc>${canonical}</loc>`),
    `Missing sitemap entry: ${path}`,
  );
  assert.ok(
    existsSync(resolve(outputRoot, path.slice(1).replace(/\.html$/, ".md"))),
  );
  for (const node of $("a[href], img[src]").toArray()) {
    const href = $(node).attr("href") ?? $(node).attr("src");
    const url = new URL(href, origin + path);
    if (url.origin !== origin) continue;
    let destination = redirects[url.pathname] ?? url.pathname;
    if (destination.endsWith("/")) destination += "index.html";
    assert.ok(
      existsSync(resolve(outputRoot, decodeURIComponent(destination.slice(1)))),
      `Broken link: ${path} -> ${href}`,
    );
    if (url.hash && html.has(destination)) {
      const ids = new Set(
        html
          .get(destination)("[id]")
          .toArray()
          .map((node) => node.attribs.id),
      );
      assert.ok(
        ids.has(decodeURIComponent(url.hash.slice(1))),
        `Broken anchor: ${path} -> ${href}`,
      );
    }
  }
}
for (const [source, target] of [
  ["../scripts/fatou-installer.sh", "install"],
  ["../scripts/fatou-installer.ps1", "install.ps1"],
  ["../fatou.schema.json", "fatou.schema.json"],
]) {
  assert.deepEqual(
    readFileSync(resolve(outputRoot, target)),
    readFileSync(resolve(docsRoot, source)),
    `${target} differs from its source`,
  );
}
const performance = readFileSync(
  resolve(outputRoot, "guide/performance.md"),
  "utf8",
);
assert.doesNotMatch(
  performance,
  /bench-chart|bench-data|\{\{ (?:benchmark|memory|lsp)-/,
);
assert.match(performance, /\| Tool \|/);
assert.match(performance, /LanguageServer\.jl/);
const home = readFileSync(resolve(outputRoot, "index.md"), "utf8");
assert.match(home, /Julia tooling, minus the runtime\./);
assert.match(home, /Find problems early/);
assert.doesNotMatch(home, /layout: home/);
const playground = html.get("/playground.html");
assert.ok(playground, "Missing playground page");
assert.equal(playground(".fatou-playground").length, 1);
assert.equal(playground("#pg-line-width").attr("value"), "92");
assert.equal(playground("#pg-indent-width").attr("value"), "4");
assert.match(
  readFileSync(resolve(outputRoot, "playground/pkg/fatou_wasm.js"), "utf8"),
  /fatou_wasm_bg\.wasm/,
);
assert.deepEqual(
  readFileSync(
    resolve(outputRoot, "playground/pkg/fatou_wasm_bg.wasm"),
  ).subarray(0, 4),
  Buffer.from([0, 97, 115, 109]),
);
const playgroundMarkdown = readFileSync(
  resolve(outputRoot, "playground.md"),
  "utf8",
);
assert.match(
  playgroundMarkdown,
  /\[Open the interactive playground\]\(\/playground\.html\)/,
);
assert.doesNotMatch(playgroundMarkdown, /<FatouPlayground/);
console.log(
  `Validated ${html.size} HTML pages, ${Object.keys(legacy).length} legacy routes, all legacy headings, internal links, Markdown, metadata, and downloads.`,
);
