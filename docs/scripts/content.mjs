import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";

export const origin = "https://fatou.dev";
export const description = "Language server, formatter, and linter for Julia";
export const docsRoot = resolve(import.meta.dirname, "..");
export const sourceRoot = resolve(docsRoot, "src");
export const outputRoot = resolve(docsRoot, ".vitepress/dist");
export const pagePath = (path) =>
  path === "index.md" ? "/" : `/${path.replace(/\.md$/, ".html")}`;

export function expandBenchmarks(source) {
  const replacements = JSON.parse(
    readFileSync(resolve(docsRoot, ".vitepress/benchmarks.json"), "utf8"),
  );
  for (const [marker, content] of Object.entries(replacements))
    source = source.replaceAll(marker, () => content);
  return source;
}

export function expandIncludes(source, file) {
  return source.replace(/<!--@include:\s*(.*?)\s*-->/g, (_, path) => {
    const target = resolve(dirname(file), path);
    return expandIncludes(readFileSync(target, "utf8"), target);
  });
}
