import { defineConfig } from "vitepress";
import { legacyAnchors } from "../legacy-anchors.mjs";
import {
  description,
  expandBenchmarks,
  origin,
  pagePath,
} from "../scripts/content.mjs";

const repository = "https://github.com/jolars/fatou";
const group = (text, pages) => ({
  text,
  items: pages.map(([text, path]) => ({ text, link: `/${path}.html` })),
});

export default defineConfig({
  title: "Fatou",
  description,
  lang: "en-US",
  srcDir: "src",
  cleanUrls: false,
  lastUpdated: true,
  head: [
    [
      "link",
      { rel: "icon", href: "/theme/favicon.svg", type: "image/svg+xml" },
    ],
    ["link", { rel: "icon", href: "/theme/favicon.png", type: "image/png" }],
    ["link", { rel: "apple-touch-icon", href: "/apple-touch-icon.png" }],
    ["meta", { property: "og:type", content: "website" }],
    ["meta", { property: "og:site_name", content: "Fatou" }],
    [
      "meta",
      { property: "og:image", content: `${origin}/images/og-image.png` },
    ],
    ["meta", { property: "og:image:type", content: "image/png" }],
    ["meta", { property: "og:image:width", content: "1200" }],
    ["meta", { property: "og:image:height", content: "630" }],
    [
      "meta",
      {
        property: "og:image:alt",
        content: `Fatou — ${description.toLowerCase()}`,
      },
    ],
    ["meta", { name: "twitter:card", content: "summary_large_image" }],
    [
      "meta",
      { name: "twitter:image", content: `${origin}/images/og-image.png` },
    ],
  ],
  transformPageData(page) {
    if (page.relativePath === "404.md") return;
    const url = origin + pagePath(page.relativePath);
    const title = page.frontmatter.title ?? page.title ?? "Fatou";
    const summary = page.description || description;
    page.frontmatter.head ??= [];
    page.frontmatter.head.push(
      ["link", { rel: "canonical", href: url }],
      ["meta", { property: "og:title", content: title }],
      ["meta", { property: "og:description", content: summary }],
      ["meta", { property: "og:url", content: url }],
      ["meta", { name: "twitter:title", content: title }],
      ["meta", { name: "twitter:description", content: summary }],
    );
  },
  markdown: {
    config(md) {
      const heading = md.renderer.rules.heading_open;
      md.renderer.rules.heading_open = (tokens, index, options, env, self) => {
        const alias = legacyAnchors[tokens[index].attrGet("id")];
        return (
          (heading?.(tokens, index, options, env, self) ??
            self.renderToken(tokens, index, options)) +
          (alias ? `<span id="${alias}" aria-hidden="true"></span>` : "")
        );
      };
      md.core.ruler.before("normalize", "benchmarks", (state) => {
        state.src = expandBenchmarks(state.src).replace(
          /<script type="application\/json" class="bench-data">([\s\S]*?)<\/script>/g,
          (_, json) =>
            `<pre class="bench-data" v-pre hidden>${md.utils.escapeHtml(json)}</pre>`,
        );
      });
    },
  },
  themeConfig: {
    logo: { src: "/images/logo-small.png", alt: "Fatou logo" },
    nav: [
      { text: "Guide", link: "/guide/getting-started.html" },
      { text: "Reference", link: "/reference/cli.html" },
      { text: "Performance", link: "/guide/performance.html" },
    ],
    sidebar: [
      group("Introduction", [
        ["Overview", "introduction"],
        ["Installation", "installation"],
        ["Getting Started", "guide/getting-started"],
      ]),
      group("Guide", [
        ["Formatting", "guide/formatting"],
        ["Linting", "guide/linting"],
        ["Configuration", "guide/configuration"],
        ["Editor Setup", "guide/editors"],
        ["Integrations", "guide/integrations"],
        ["Performance", "guide/performance"],
        ["Comparison", "guide/comparison"],
      ]),
      group("Reference", [
        ["CLI Reference", "reference/cli"],
        ["Configuration Reference", "reference/configuration"],
        ["Lint Rules", "reference/rules"],
      ]),
      group("Development", [["Contributing", "development/contributing"]]),
    ],
    search: { provider: "local" },
    socialLinks: [{ icon: "github", link: repository }],
    editLink: { pattern: `${repository}/edit/main/docs/src/:path` },
    footer: { message: "Released under the MIT License." },
  },
});
