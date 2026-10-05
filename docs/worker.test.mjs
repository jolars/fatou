import assert from "node:assert/strict";
import { test } from "node:test";
import worker, { acceptsMarkdown } from "./worker.mjs";

test("negotiates explicit markdown, including a weighted media range", () => {
  assert.equal(acceptsMarkdown("text/html, text/markdown;q=0.8"), true);
  assert.equal(acceptsMarkdown("TEXT/MARKDOWN; charset=utf-8"), true);
  assert.equal(acceptsMarkdown("text/markdown;q=0"), false);
  assert.equal(acceptsMarkdown("text/html, */*"), false);
});

test("serves Markdown at the page URL and leaves other requests on the origin", async () => {
  const fetched = [];
  globalThis.fetch = async (request) => {
    fetched.push(request.url);
    return new Response(
      request.url.endsWith(".md") ? "# Fatou\n" : "<h1>Fatou</h1>",
      {
        headers: {
          "Content-Type": request.url.endsWith(".md")
            ? "text/plain"
            : "text/html",
        },
      },
    );
  };

  const markdown = await worker.fetch(
    new Request("https://fatou.dev/guide/editors.html", {
      headers: { Accept: "text/markdown" },
    }),
  );
  assert.equal(await markdown.text(), "# Fatou\n");
  assert.match(markdown.headers.get("Content-Type"), /^text\/markdown/);
  assert.equal(markdown.headers.get("Vary"), "Accept");
  assert.equal(fetched[0], "https://fatou.dev/guide/editors.md");

  const html = await worker.fetch(new Request("https://fatou.dev/"));
  assert.equal(await html.text(), "<h1>Fatou</h1>");
  assert.equal(html.headers.get("Vary"), "Accept");
  assert.equal(fetched[1], "https://fatou.dev/");

  await worker.fetch(
    new Request("https://fatou.dev/install", {
      headers: { Accept: "text/markdown" },
    }),
  );
  assert.equal(fetched[2], "https://fatou.dev/install");
});

test("falls back to origin HTML when a Markdown page is absent", async () => {
  globalThis.fetch = async (request) =>
    request.url.endsWith(".md")
      ? new Response("missing", { status: 404 })
      : new Response("<h1>Page</h1>", { headers: { Vary: "Accept-Encoding" } });
  const response = await worker.fetch(
    new Request("https://fatou.dev/moved.html", {
      headers: { Accept: "text/markdown" },
    }),
  );
  assert.equal(await response.text(), "<h1>Page</h1>");
  assert.equal(response.headers.get("Vary"), "Accept-Encoding, Accept");
});
