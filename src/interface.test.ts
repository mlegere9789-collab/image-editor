import { test } from "node:test";
import assert from "node:assert/strict";
import { applyInterface, INTERFACE_DEFAULTS, parseInterface } from "./interface.ts";

test("stored preferences parse, and anything missing or unknown falls back", () => {
  assert.deepEqual(parseInterface(null), INTERFACE_DEFAULTS);
  assert.deepEqual(parseInterface("not json"), INTERFACE_DEFAULTS);
  assert.deepEqual(parseInterface("42"), INTERFACE_DEFAULTS);
  assert.deepEqual(parseInterface(JSON.stringify({ theme: "light" })), {
    theme: "light",
    highlight: "blue",
    uiSize: "medium",
  });
  assert.deepEqual(
    parseInterface(JSON.stringify({ theme: "neon", highlight: "grey", uiSize: "huge" })),
    { theme: "dark", highlight: "grey", uiSize: "medium" },
  );
  assert.deepEqual(
    parseInterface(JSON.stringify({ theme: "lightest", highlight: "grey", uiSize: "large" })),
    { theme: "lightest", highlight: "grey", uiSize: "large" },
  );
});

test("applying preferences sets the attributes the stylesheet keys on", () => {
  const root = { dataset: {} as Record<string, string | undefined> };
  applyInterface(root, { theme: "light", highlight: "grey", uiSize: "small" });
  assert.deepEqual(root.dataset, { theme: "light", highlight: "grey", uiSize: "small" });
  applyInterface(root, INTERFACE_DEFAULTS);
  assert.deepEqual(root.dataset, { theme: "dark", highlight: "blue", uiSize: "medium" });
});
