import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import test from "node:test";
import init, {
  check,
  format,
} from "../.vitepress/dist/playground/pkg/fatou_wasm.js";
import { outputRoot } from "../scripts/content.mjs";

await init({
  module_or_path: await readFile(
    resolve(outputRoot, "playground/pkg/fatou_wasm_bg.wasm"),
  ),
});

test("browser bindings format Julia and enforce options through JavaScript", () => {
  assert.equal(format("x=1"), "x = 1\n");
  assert.equal(format("x=1\n"), "x = 1\n");
  assert.equal(format(""), "");
  const source = "function f(x,y)\n(first=x,second=y,third=x+y)\nend\n";
  const formatted = format(source, 30, 8);
  assert.match(formatted, /\n {8}\S/);
  assert.equal(format(formatted, 30, 8), formatted);
  for (const invalid of [0, -1, 1001, 1.5, 2 ** 32 + 92, NaN, Infinity]) {
    assert.throws(() => format("x=1", invalid), /line-width/);
    assert.throws(() => format("x=1", undefined, invalid), /indent-width/);
  }
  assert.throws(() => format("function f("), /parse errors/);
  assert.equal(format("x=1\r\n"), "x = 1\r\n");
});

test("browser diagnostics use UTF-16 positions and can be freed", () => {
  assert.deepEqual(check("α=1\nβ=α+2\n"), []);
  const diagnostics = check("# 😀é\r\n)");
  try {
    assert.ok(diagnostics.length > 0);
    for (const diagnostic of diagnostics) {
      assert.ok(diagnostic.message.length > 0);
      assert.equal(diagnostic.start, 7);
      assert.equal(diagnostic.end, 8);
      assert.equal(diagnostic.line, 2);
      assert.equal(diagnostic.column, 1);
    }
  } finally {
    for (const diagnostic of diagnostics) diagnostic.free();
  }
  const incomplete = check("f(");
  try {
    assert.ok(incomplete.length > 0);
    for (const diagnostic of incomplete) {
      assert.ok(diagnostic.start <= diagnostic.end);
      assert.ok(diagnostic.end <= 2);
    }
  } finally {
    for (const diagnostic of incomplete) diagnostic.free();
  }
});
