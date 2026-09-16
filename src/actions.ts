// The Actions panel's model: what a recording keeps of each command the
// app ran, and how playback aims it at whatever is selected then.
//
// A recorded command keeps its arguments as they were sent, except that
// the selected layer's id becomes the token `"$selected"` — the way
// Photoshop's actions target "the current layer" rather than the layer
// that happened to be selected while recording — and the progress
// channel a long command carries is dropped, since it belongs to the
// one run it was made for. Playback resolves the token to the layer
// selected at that moment, and refuses to run a step that needs one
// when nothing is selected.

/** The selected layer's id, as a recording stores it. */
export const SELECTED = "$selected";

/** Commands a recording never keeps: they change which document is open, or are history itself. */
export const NON_RECORDABLE: ReadonlySet<string> = new Set([
  "open_document",
  "new_document",
  "open_project",
  "import_project_bytes",
  "recover_autosave",
  "undo",
  "redo",
  "checkpoint",
  "cancel_operation",
]);

/** Whether a command the app ran belongs in a recording. */
export function isRecordable(command: string): boolean {
  return !NON_RECORDABLE.has(command);
}

/** Arguments as recorded: `id` values equal to the selected layer become `SELECTED`; `onProgress` goes. */
export function recordArgs(
  args: Record<string, unknown>,
  selectedId: number | null,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(args)) {
    if (key === "onProgress") continue;
    out[key] = selectedId !== null && key === "id" && value === selectedId ? SELECTED : value;
  }
  return out;
}

/**
 * Arguments as played: every `SELECTED` becomes the layer selected now.
 * Throws when a step needs a selected layer and there is none.
 */
export function playArgs(
  args: Record<string, unknown>,
  selectedId: number | null,
): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(args)) {
    if (value === SELECTED) {
      if (selectedId === null) throw new Error("This step needs a selected layer.");
      out[key] = selectedId;
    } else {
      out[key] = value;
    }
  }
  return out;
}

/** One step of an action, as `actions.rs` stores it. */
export type ActionStep =
  | { kind: "command"; command: string; args: Record<string, unknown> }
  | { kind: "stop"; message: string };

/** A recorded action, as `actions.rs` stores it. */
export type RecordedAction = {
  name: string;
  steps: ActionStep[];
};

/** A step as the panel lists it: the command with its arguments, or the stop's message. */
export function describeStep(step: ActionStep): string {
  if (step.kind === "stop") return `Stop: ${step.message}`;
  const args = Object.entries(step.args)
    .filter(([key]) => key !== "id")
    .map(
      ([key, value]) =>
        `${key}=${typeof value === "object" ? JSON.stringify(value) : String(value)}`,
    )
    .join(", ");
  return args ? `${step.command} (${args})` : step.command;
}

/** The file name Batch writes for `source` under the output folder. */
export function batchOutputName(source: string): string {
  const base = source.split(/[\\/]/).pop() ?? source;
  return base.replace(/\.[Pp][Nn][Gg]$/, "") + ".png";
}
