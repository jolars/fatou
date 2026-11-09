# Documentation site

VitePress builds `src/` into `.vitepress/dist/`. Cloudflare Workers serves that
complete output through the existing `fatou-docs-markdown` Worker. The Worker
uses its `ASSETS` binding for Markdown negotiation and no longer fetches GitHub
Pages as an origin.

## Local development

Use the repository's devenv shell, which includes Node.js, pnpm, `wasm-pack`,
and Rust with the `wasm32-unknown-unknown` target:

```sh
pnpm --dir docs install --frozen-lockfile
task docs-preview
```

Build before previewing the production Worker:

```sh
task docs-build
task docs-worker
```

In another terminal, run `task docs-check`. The build validates internal links,
all routes and headings captured from the previous mdBook build, metadata,
Markdown exports, and exact installer and schema bytes. The Worker check also
validates content negotiation, GET and HEAD requests, redirects, queries, real
404 responses, content types, and CORS. Restart the Worker preview after a
rebuild so it loads the new asset map.

Development and production builds first run `playground:wasm` to compile the
unpublished `fatou-wasm` crate and generate browser bindings under
`src/public/playground/pkg/`. These generated files are ignored. After changing
Rust code during development, run `pnpm --dir docs playground:wasm` from the
repository root and reload the playground. After a production build, run
`pnpm --dir docs test:wasm` to check the actual JavaScript and Wasm exports.
`pnpm --dir docs test` runs the Worker and playground settings unit tests
without requiring a site build.

`prepare:content` runs the existing Rust rule generator and benchmark renderer.
It reads `bench/results.json` and `bench/memory.json`; it never measures
benchmarks or invokes Julia. The Rust build script still generates the CLI
reference. Keep generated reference pages authoritative at their generators.

Static files live under `src/public/`. The installers and configuration schema
are copied from their canonical repository sources during preparation and are
ignored there. VitePress configuration owns navigation and page metadata;
`scripts/publish-content.mjs` publishes readable Markdown and the sitemap with
git-derived modification dates. The homepage Markdown includes the hero and
feature descriptions. Chart data tables remain available without JavaScript and
in Markdown.

## Published URLs

Pages retain `.html` paths, including `/introduction.html` and `/index.html`.
The homepage canonical URL remains `https://fatou.dev/`. `redirects.mjs` retains
historical aliases from mdBook's configuration, and `legacy-anchors.mjs` adds
aliases for headings whose VitePress IDs differ. The fixture in
`tests/legacy-routes.json` records a fresh mdBook build before migration. Do not
regenerate it from VitePress.

The Worker preserves query parameters on redirects. Destination fragments carry
through unchanged; browser-only fragments are preserved by HTTP redirects
without needing a server rewrite. Unknown routes return 404, including unknown
Markdown paths. Direct `.md` downloads and GET/HEAD requests with
`Accept: text/markdown` work at page URLs. HTML and negotiated Markdown vary by
`Accept`, and HTML validators and ranges are removed before fetching the
Markdown representation.

## Deployment and initial cutover

The Documentation workflow builds and checks pull requests without Cloudflare
credentials. Root release tags (`v*`) and manual workflow dispatch build the
same site and deploy the checked artifact. Only the deployment step receives
`CLOUDFLARE_ACCOUNT_ID` and `CLOUDFLARE_API_TOKEN` from GitHub secrets.
Production deployments are serialized. The workflow checks the live site after
deploying. Parser, formatter, and editor release tags do not publish the site.

Before the first production deployment:

1. Inspect the `fatou.dev` zone in the account identified by the GitHub secret.
   Record the current apex DNS records, `fatou.dev/*` Worker route, and current
   Worker deployment/version so the GitHub Pages origin can be restored.
2. Ensure the API token can deploy Workers, edit Worker routes, and attach a
   Custom Domain in that zone. The configuration retains the existing Worker
   name and changes its wildcard overlay route into the `fatou.dev` Custom
   Domain, making Workers the origin.
3. Remove only apex DNS records that conflict with attaching that Custom Domain.
   Cloudflare cannot attach it over an existing CNAME. Do not change unrelated
   DNS records. Then dispatch Documentation from the branch containing this
   migration, or publish the next root release tag.
4. Verify the deployed homepage, deep links, legacy redirects, Markdown,
   installers, schema, and missing routes. The workflow runs these HTTP checks
   automatically; also inspect navigation, search, charts, and mobile layout.
   Confirm the Custom Domain points to `fatou-docs-markdown`, and that the old
   wildcard overlay is gone. Disable GitHub Pages only after verification.

If domain attachment fails, inspect the reported conflict or permission error
before retrying. Restore the previous DNS records, Worker version, and wildcard
route if the origin needs to be rolled back. Keep the old GitHub Pages
publication available until the new origin has been verified.
