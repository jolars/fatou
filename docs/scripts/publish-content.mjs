import { execFileSync } from "node:child_process";
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { load } from "cheerio";
import { createMarkdownRenderer } from "vitepress";
import {
  expandBenchmarks,
  expandIncludes,
  origin,
  outputRoot,
  pagePath,
  sourceRoot,
} from "./content.mjs";

export function pages(root = sourceRoot, prefix = "") {
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    const path = prefix + entry.name;
    if (entry.name === "public") return [];
    return entry.isDirectory()
      ? pages(resolve(root, entry.name), path + "/")
      : path.endsWith(".md")
        ? [path]
        : [];
  });
}

function chartToMarkdown(block) {
  const $ = load(block, null, false);
  $("figure, noscript, summary").remove();
  $("table").each((_, table) => {
    const rows = $(table)
      .find("tr")
      .toArray()
      .map(
        (row) =>
          "| " +
          $(row)
            .find("th, td")
            .toArray()
            .map((cell) => $(cell).text().trim().replaceAll("|", "\\|"))
            .join(" | ") +
          " |",
      );
    if (rows.length)
      rows.splice(
        1,
        0,
        "| " +
          $(table)
            .find("tr")
            .first()
            .find("th, td")
            .toArray()
            .map(() => "---")
            .join(" | ") +
          " |",
      );
    const caption = $(table).find("caption").text();
    $(table).replaceWith(
      "\n" + (caption ? `**${caption}**\n\n` : "") + rows.join("\n") + "\n",
    );
  });
  $("h3, h4").each((_, heading) =>
    $(heading).replaceWith(
      `\n${"#".repeat(Number(heading.tagName[1]))} ${$(heading).text()}\n`,
    ),
  );
  $("p").each((_, p) => $(p).replaceWith(`\n${$(p).text()}\n`));
  return "\n" + $.root().text().trim() + "\n";
}

const renderer = await createMarkdownRenderer(sourceRoot);
const sitemap = [];
for (const path of pages()) {
  const file = resolve(sourceRoot, path);
  let source = expandIncludes(readFileSync(file, "utf8"), file);
  source = expandBenchmarks(source);
  const env = {};
  renderer.render(source, env);
  const frontmatter = env.frontmatter ?? {};
  source = source.replace(/^---\n[\s\S]*?\n---\n/, "");
  if (frontmatter.layout === "home") {
    const { hero, features } = frontmatter;
    source =
      `# ${hero.name}\n\n${hero.text}\n\n${hero.tagline}\n\n` +
      hero.actions
        .map((action) => `- [${action.text}](${action.link})`)
        .join("\n") +
      "\n\n" +
      features
        .map((feature) => `## ${feature.title}\n\n${feature.details}\n`)
        .join("\n") +
      source;
  }
  source = source.replace(
    /<div class="bench-chart-block">\n([\s\S]*?)\n<\/div>/g,
    (block) => chartToMarkdown(block),
  );
  source = source
    .replace(
      /<FatouPlayground\s*\/>/g,
      "[Open the interactive playground](/playground.html).",
    )
    .replace(/<span id="[^"]+"><\/span>\n?/g, "")
    .replace(
      /<img src="([^"]+)" alt="([^"]+)"[^>]*\/?\s*>/g,
      (_, src, alt) => `![${alt}](${src})`,
    );
  source = source.replace(/\]\(([^)]+)\.md(#[^)]*)?\)/g, "]($1.html$2)");
  const destination = resolve(outputRoot, path);
  mkdirSync(dirname(destination), { recursive: true });
  writeFileSync(destination, source);
  const date = execFileSync("git", ["log", "-1", "--format=%cs", "--", file], {
    encoding: "utf8",
  }).trim();
  sitemap.push(
    `<url><loc>${origin}${pagePath(path)}</loc>${date ? `<lastmod>${date}</lastmod>` : ""}</url>`,
  );
}
writeFileSync(
  resolve(outputRoot, "sitemap.xml"),
  `<?xml version="1.0" encoding="UTF-8"?>\n<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n${sitemap.join("\n")}\n</urlset>\n`,
);
