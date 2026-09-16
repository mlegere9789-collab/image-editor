import { test } from "node:test";
import assert from "node:assert/strict";
import { BRUSH_TOOLS, optionsRule, PEOPLE_TOOLS, TIP_TOOLS } from "./optionsBar.ts";

test("the options rule names the active tool and nothing else", () => {
  assert.equal(
    optionsRule("brush"),
    'header.toolbar [data-tool-option]:not([data-tool-option~="brush"]){display:none!important}',
  );
  // A tool id is a plain identifier; anything else is stripped so the rule cannot be broken out of.
  assert.equal(
    optionsRule('x"]{}'),
    'header.toolbar [data-tool-option]:not([data-tool-option~="x"]){display:none!important}',
  );
});

test("the shared-option tool lists are distinct identifiers", () => {
  for (const list of [BRUSH_TOOLS, TIP_TOOLS, PEOPLE_TOOLS]) {
    assert.equal(new Set(list).size, list.length);
    for (const id of list) assert.match(id, /^[a-zA-Z]+$/);
  }
  assert.ok(BRUSH_TOOLS.includes("brush") && BRUSH_TOOLS.includes("healingBrush"));
  assert.ok(!(BRUSH_TOOLS as readonly string[]).includes("move"));
});
