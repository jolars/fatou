import assert from "node:assert/strict";
import { test } from "node:test";
import worker, { acceptsMarkdown } from "./worker.mjs";

function assets(handler) {
  return { ASSETS: { fetch: handler } };
}

test("negotiates explicit Markdown with nonzero weights", () => {
  assert.equal(acceptsMarkdown("text/html, text/markdown;q=0.8"), true);
  assert.equal(acceptsMarkdown("TEXT/MARKDOWN; charset=utf-8"), true);
  assert.equal(acceptsMarkdown("text/markdown;q=0"), false);
  assert.equal(acceptsMarkdown("text/html, */*"), false);
});

test("serves GET and HEAD Markdown through the assets binding", async () => {
  for (const method of ["GET", "HEAD"]) {
    const env = assets(async (request) => {
      assert.equal(new URL(request.url).pathname, "/guide/editors.md");
      assert.equal(request.method, method);
      for (const header of [
        "Accept",
        "If-None-Match",
        "If-Modified-Since",
        "Range",
        "If-Range",
      ])
        assert.equal(request.headers.has(header), false);
      return new Response(method === "HEAD" ? null : "# Editors\n");
    });
    const response = await worker.fetch(
      new Request("https://fatou.dev/guide/editors.html", {
        method,
        headers: {
          Accept: "text/markdown",
          "If-None-Match": '"html"',
          Range: "bytes=0-3",
        },
      }),
      env,
    );
    assert.equal(response.status, 200);
    assert.equal(response.headers.get("Vary"), "Accept");
    assert.match(response.headers.get("Content-Type"), /^text\/markdown/);
    assert.equal(await response.text(), method === "HEAD" ? "" : "# Editors\n");
  }
});

test("keeps HTML, downloads, and missing pages on the assets binding", async () => {
  const env = assets(async (request) => {
    const path = new URL(request.url).pathname;
    return new Response(path, {
      status: path.startsWith("/missing") ? 404 : 200,
      headers: { Vary: "Accept-Encoding" },
    });
  });
  for (const path of ["/", "/install", "/missing.html"]) {
    const response = await worker.fetch(
      new Request(`https://fatou.dev${path}`),
      env,
    );
    assert.equal(await response.text(), path === "/" ? "/index.html" : path);
    assert.equal(response.status, path === "/missing.html" ? 404 : 200);
  }
  const response = await worker.fetch(new Request("https://fatou.dev/"), env);
  assert.equal(response.headers.get("Vary"), "Accept-Encoding, Accept");
  assert.match(response.headers.get("Link"), /<\/index.md>/);
});

test("redirects legacy routes while preserving queries and fragments", async () => {
  for (const path of ["/getting-started.html", "/comparison/formatters.html"]) {
    const response = await worker.fetch(
      new Request(`https://fatou.dev${path}?from=old#installation`),
      assets(() => {
        throw Error("unexpected asset lookup");
      }),
    );
    assert.equal(response.status, 301);
    const destination = new URL(response.headers.get("Location"));
    assert.equal(
      destination.pathname,
      path.startsWith("/comparison")
        ? "/guide/comparison.html"
        : "/guide/getting-started.html",
    );
    assert.equal(destination.search, "?from=old");
    assert.equal(destination.hash, "#installation");
  }
});

test("falls back to HTML when Markdown is missing", async () => {
  const response = await worker.fetch(
    new Request("https://fatou.dev/missing.html", {
      headers: { Accept: "text/markdown" },
    }),
    assets(
      async (request) =>
        new Response(request.url.endsWith(".md") ? "no Markdown" : "no HTML", {
          status: 404,
        }),
    ),
  );
  assert.equal(response.status, 404);
  assert.equal(await response.text(), "no HTML");
});
