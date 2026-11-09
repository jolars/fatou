import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { redirects } from "../redirects.mjs";
import { docsRoot } from "./content.mjs";

const origin = process.argv[2] ?? "http://127.0.0.1:8787";
const legacy = JSON.parse(
  readFileSync(resolve(docsRoot, "tests/legacy-routes.json"), "utf8"),
);
for (const path of ["/", "/playground.html", ...Object.keys(legacy)]) {
  const response = await fetch(origin + path);
  assert.equal(response.status, 200, path);
  assert.match(response.headers.get("Content-Type"), /text\/html/);
  for (const method of ["GET", "HEAD"]) {
    const markdown = await fetch(origin + path, {
      method,
      headers: {
        Accept: "text/markdown",
        "If-None-Match": '"html"',
        Range: "bytes=0-3",
      },
    });
    assert.equal(markdown.status, 200, `${method} ${path}`);
    assert.match(markdown.headers.get("Content-Type"), /text\/markdown/);
    assert.match(markdown.headers.get("Vary"), /Accept/);
    if (method === "HEAD") assert.equal(await markdown.text(), "");
  }
}
for (const [path, target] of Object.entries(redirects)) {
  const response = await fetch(`${origin}${path}?from=old`, {
    redirect: "manual",
  });
  assert.equal(response.status, 301);
  assert.equal(response.headers.get("Location"), `${origin}${target}?from=old`);
  assert.equal((await fetch(response.headers.get("Location"))).status, 200);
}
for (const path of ["/missing", "/missing.html", "/missing.md", "/missing/"]) {
  for (const accept of ["text/html", "text/markdown"]) {
    assert.equal(
      (await fetch(origin + path, { headers: { Accept: accept } })).status,
      404,
      path,
    );
  }
}
for (const [path, source, type] of [
  ["/install", "../scripts/fatou-installer.sh", "application/octet-stream"],
  [
    "/install.ps1",
    "../scripts/fatou-installer.ps1",
    "application/octet-stream",
  ],
  ["/fatou.schema.json", "../fatou.schema.json", "application/json"],
]) {
  const response = await fetch(origin + path);
  assert.equal(response.status, 200);
  assert.ok(response.headers.get("Content-Type").startsWith(type));
  assert.equal(response.headers.get("Access-Control-Allow-Origin"), "*");
  assert.deepEqual(
    Buffer.from(await response.arrayBuffer()),
    readFileSync(resolve(docsRoot, source)),
  );
}
const direct = await fetch(`${origin}/index.md`);
assert.match(direct.headers.get("Content-Type"), /text\/markdown/);
const playgroundJs = await fetch(`${origin}/playground/pkg/fatou_wasm.js`);
assert.equal(playgroundJs.status, 200);
assert.match(
  playgroundJs.headers.get("Content-Type"),
  /(?:application|text)\/javascript/,
);
assert.match(await playgroundJs.text(), /fatou_wasm_bg\.wasm/);
const playgroundWasm = await fetch(
  `${origin}/playground/pkg/fatou_wasm_bg.wasm`,
);
assert.equal(playgroundWasm.status, 200);
assert.match(playgroundWasm.headers.get("Content-Type"), /application\/wasm/);
assert.deepEqual(
  Buffer.from(await playgroundWasm.arrayBuffer()).subarray(0, 4),
  Buffer.from([0, 97, 115, 109]),
);
console.log(
  "Validated Worker HTML, GET/HEAD Markdown negotiation, redirects, queries, 404s, download bytes, content types, CORS, and playground assets.",
);
