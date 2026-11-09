import assert from "node:assert/strict";
import test from "node:test";
import {
  readSettings,
  settingsUrl,
} from "../.vitepress/theme/playground-settings.mjs";

test("missing or invalid URL settings use Fatou's defaults", () => {
  const defaults = { lineWidth: 92, indentWidth: 4 };
  assert.deepEqual(readSettings(""), defaults);
  assert.deepEqual(readSettings("?lw=0&iw=1001&code=private"), defaults);
  for (const width of ["", "-1", "2.5", "NaN", "Infinity", "0x20"]) {
    assert.equal(readSettings(`?lw=${width}`).lineWidth, 92);
  }
  assert.deepEqual(readSettings("?lw=40&iw=2"), {
    lineWidth: 40,
    indentWidth: 2,
  });
});

test("share links contain only settings and round-trip them", () => {
  const settings = { lineWidth: 100, indentWidth: 8 };
  const url = settingsUrl(
    "https://fatou.dev/playground.html?code=private#source",
    settings,
  );
  assert.equal(url.href, "https://fatou.dev/playground.html?lw=100&iw=8");
  assert.deepEqual(readSettings(url.search), settings);
});
