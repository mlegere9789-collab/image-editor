import { test } from "node:test";
import assert from "node:assert/strict";
import {
  buildMenuTree,
  commandKey,
  flattenMenuTree,
  menuHint,
  menuPath,
  menuShortcut,
  type MenuColor,
  type MenuEntry,
} from "./menuBar.ts";

const noop = () => {};
const entry = (label: string, title: string, disabled = false): MenuEntry => ({
  label,
  title,
  disabled,
  run: noop,
});

test("menuPath reads the menu above the command and drops the command's own name", () => {
  assert.deepEqual(menuPath("Edit > Transform > Rotate (any angle, selected layer)"), [
    "Edit",
    "Transform",
  ]);
  assert.deepEqual(menuPath("Edit > Fill"), ["Edit"]);
  assert.deepEqual(menuPath("Image > Image Rotation > 90° Clockwise"), ["Image", "Image Rotation"]);
  assert.deepEqual(menuPath("Layer > New Fill Layer > Solid Color"), ["Layer", "New Fill Layer"]);
});

test("menuPath stops at the description, whatever punctuation starts it", () => {
  assert.deepEqual(menuPath("Edit > Content-Aware Fill: replace the selection"), ["Edit"]);
  assert.deepEqual(menuPath("Filter > Generative Fill (AI) — on-device"), ["Filter"]);
  assert.deepEqual(menuPath("Filter > Sharpen > Unsharp Mask -- the classic"), [
    "Filter",
    "Sharpen",
  ]);
  assert.deepEqual(menuPath("View > Gamut Warning: flags any pixel whose split (naive) exceeds"), [
    "View",
  ]);
});

test("menuPath folds submenu-rooted titles under their Photoshop menu", () => {
  assert.deepEqual(menuPath("Filter Gallery > Artistic > Poster Edges"), [
    "Filter",
    "Filter Gallery",
    "Artistic",
  ]);
  assert.deepEqual(menuPath("Neural Filters > Super Zoom — real on-device"), [
    "Filter",
    "Neural Filters",
  ]);
  assert.deepEqual(menuPath("Camera Raw Filter > Clarity"), ["Filter", "Camera Raw Filter"]);
  assert.deepEqual(menuPath("Color Settings > OpenColorIO Panel: show or hide"), [
    "Edit",
    "Color Settings",
  ]);
  assert.deepEqual(menuPath("Filter Gallery > Blur Gallery > Spin Blur"), [
    "Filter",
    "Blur Gallery",
  ]);
});

test("menuPath names no menu for tools, contextual buttons, and prose that merely mentions one", () => {
  assert.equal(menuPath(""), null);
  assert.equal(menuPath("Undo (Ctrl/Cmd+Z)"), null);
  assert.equal(menuPath("Remember the current document state as the History Brush's source"), null);
  assert.equal(menuPath("Load an alpha channel made by Image > Calculations"), null);
  assert.equal(menuPath("Discover: search the Toolbox by name"), null);
});

test("menuShortcut and menuHint read the rest of the title", () => {
  assert.equal(menuShortcut("Edit > Undo (Ctrl/Cmd+Z)"), "Ctrl/Cmd+Z");
  assert.equal(menuShortcut("Select > All (Ctrl/Cmd+A): every pixel"), "Ctrl/Cmd+A");
  assert.equal(menuShortcut("Edit > Fill"), null);
  assert.equal(
    menuShortcut("Edit > Copy Merged (Shift+Ctrl+C: copies every visible layer)"),
    "Shift+Ctrl+C",
  );
  assert.equal(
    menuHint("Edit > Content-Aware Fill: replace the selection with the mean"),
    "replace the selection with the mean",
  );
  assert.equal(menuHint("Filter > Generative Fill (AI) — on-device"), "on-device");
  assert.equal(menuHint("Edit > Fill"), "");
});

test("buildMenuTree keeps Photoshop's menu order and the toolbar's order within a menu", () => {
  const tree = buildMenuTree([
    entry("Rotate…", "Edit > Transform > Rotate (any angle)"),
    entry("Fill…", "Edit > Fill"),
    entry("Brush", "", false),
    entry("New…", "File > New"),
    entry("Scale…", "Edit > Transform > Scale", true),
    entry("Poster Edges…", "Filter Gallery > Artistic > Poster Edges"),
    entry("Set Source", "Remember the current document state"),
  ]);
  assert.deepEqual(
    tree.map((menu) => menu.label),
    ["File", "Edit", "Image", "Layer", "Type", "Select", "Filter", "View", "Window", "Help"],
  );
  const edit = tree[1];
  assert.deepEqual(
    edit.items.map((item) => item.label),
    ["Fill…", "Transform"],
  );
  const transform = edit.items[1];
  assert.equal(transform.kind, "group");
  if (transform.kind !== "group") return;
  assert.deepEqual(transform.path, ["Edit", "Transform"]);
  assert.deepEqual(
    transform.items.map((item) => item.kind === "command" && item.disabled),
    [false, true],
  );
  const filter = tree[6];
  assert.equal(filter.items.length, 1);
  assert.equal(filter.items[0].label, "Filter Gallery");
  assert.equal(flattenMenuTree(tree).length, 5);
  assert.equal(
    flattenMenuTree(tree)
      .map((command) => command.label)
      .join(","),
    "New…,Fill…,Rotate…,Scale…,Poster Edges…",
  );
});

test("buildMenuTree runs the command it was built from", () => {
  let ran = 0;
  const tree = buildMenuTree([
    {
      label: "Undo",
      title: "Edit > Undo (Ctrl/Cmd+Z)",
      disabled: false,
      run: () => {
        ran += 1;
      },
    },
  ]);
  const undo = flattenMenuTree(tree)[0];
  undo.run();
  assert.equal(ran, 1);
  assert.equal(undo.shortcut, "Ctrl/Cmd+Z");
});

test("buildMenuTree leaves out hidden commands and the submenus that empties", () => {
  const entries = [
    entry("Rotate…", "Edit > Transform > Rotate (any angle)"),
    entry("Fill…", "Edit > Fill"),
    entry("Skew…", "Edit > Transform > Skew"),
  ];
  const all = flattenMenuTree(buildMenuTree(entries));
  assert.deepEqual(all.map(commandKey), [
    "Edit > Fill…",
    "Edit > Transform > Rotate…",
    "Edit > Transform > Skew…",
  ]);
  const some = buildMenuTree(entries, new Set(["Edit > Transform > Rotate…"]));
  assert.deepEqual(flattenMenuTree(some).map(commandKey), [
    "Edit > Fill…",
    "Edit > Transform > Skew…",
  ]);
  const none = buildMenuTree(
    entries,
    new Set(["Edit > Transform > Rotate…", "Edit > Transform > Skew…"]),
  );
  assert.deepEqual(
    none[1].items.map((item) => item.label),
    ["Fill…"],
  );
});

test("buildMenuTree orders Image > Adjustments the way Photoshop does and appends the rest", () => {
  const tree = buildMenuTree([
    entry("Threshold…", "Image > Adjustments > Threshold"),
    entry("Brightness/Contrast…", "Image > Adjustments > Brightness/Contrast"),
    entry("Equalize from Sel.", "Image > Adjustments > Equalize > Equalize from Selection"),
    entry("Levels…", "Image > Adjustments > Levels"),
    entry("Canvas Size…", "Image > Canvas Size: grow or shrink"),
  ]);
  const image = tree[2];
  assert.deepEqual(
    image.items.map((item) => item.label),
    ["Adjustments", "Canvas Size…"],
  );
  const adjustments = image.items[0];
  assert.equal(adjustments.kind, "group");
  if (adjustments.kind !== "group") return;
  assert.deepEqual(
    adjustments.items.map((item) => item.label),
    ["Brightness/Contrast…", "Levels…", "Threshold…", "Equalize"],
  );
});

test("buildMenuTree labels each command with its own Menu Color, and leaves the rest uncoloured", () => {
  const entries = [
    entry("Rotate…", "Edit > Transform > Rotate (any angle)"),
    entry("Fill…", "Edit > Fill"),
    entry("Skew…", "Edit > Transform > Skew"),
  ];
  const colors = new Map<string, MenuColor>([
    ["Edit > Transform > Rotate…", "red"],
    ["Edit > Fill…", "blue"],
  ]);
  const tree = buildMenuTree(entries, new Set(), colors);
  assert.deepEqual(
    flattenMenuTree(tree).map((command) => [commandKey(command), command.color]),
    [
      ["Edit > Fill…", "blue"],
      ["Edit > Transform > Rotate…", "red"],
      ["Edit > Transform > Skew…", null],
    ],
  );
});

test("buildMenuTree leaves every command uncoloured when no colours are given", () => {
  const tree = buildMenuTree([entry("Fill…", "Edit > Fill")]);
  assert.deepEqual(
    flattenMenuTree(tree).map((command) => command.color),
    [null],
  );
});
