//! AI Assisted Editor: a conversation that edits the document. The
//! desktop app sends the conversation so far and a summary of the open
//! document; the server asks Claude (Anthropic's Messages API, with the
//! user's own API key) with this app's own commands offered as tools,
//! and returns the reply's text and the tool calls as actions the app
//! runs through its ordinary command path -- undoable, checkpointed,
//! exactly as if clicked. The app reports each action's result back as
//! a tool result and the conversation continues. With no key at all, a
//! rule-based reader handles the plainest requests ("make it brighter",
//! "blur it a little", "select the subject") so the assistant is never
//! entirely absent.
//!
//! The tool catalogue is a curated subset of the app's commands: the
//! adjustments, filters, selection and generative commands a request in
//! plain words usually wants, each with the exact argument names the
//! Tauri command takes. Commands that act on a layer take the selected
//! layer, which the app fills in (`needs_layer`).

use std::io::Read;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const DEFAULT_MODEL: &str = "claude-opus-5";
pub const ANTHROPIC_URL: &str = "https://api.anthropic.com";

/// One command the assistant may call.
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
    pub needs_layer: bool,
    pub schema: fn() -> Value,
}

fn no_args() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

macro_rules! schema {
    ($($body:tt)*) => { || json!($($body)*) };
}

pub const COMMANDS: &[Command] = &[
    Command { name: "brightness_contrast", description: "Adjust brightness and contrast of the selected layer. Both range -150..150; 0 is no change.", needs_layer: true, schema: schema!({"type":"object","properties":{"brightness":{"type":"integer"},"contrast":{"type":"integer"}},"required":["brightness","contrast"],"additionalProperties":false}) },
    Command { name: "hue_saturation", description: "Shift hue (-180..180), saturation (-100..100) and lightness (-100..100) of the selected layer.", needs_layer: true, schema: schema!({"type":"object","properties":{"hue":{"type":"integer"},"saturation":{"type":"integer"},"lightness":{"type":"integer"}},"required":["hue","saturation","lightness"],"additionalProperties":false}) },
    Command { name: "vibrance", description: "Vibrance (-100..100, boosts muted colours) and saturation (-100..100) of the selected layer.", needs_layer: true, schema: schema!({"type":"object","properties":{"vibrance":{"type":"integer"},"saturation":{"type":"integer"}},"required":["vibrance","saturation"],"additionalProperties":false}) },
    Command { name: "exposure", description: "Exposure in hundredths of a stop (-2000..2000), offset (-500..500, hundredths), gamma (10..999, hundredths; 100 is neutral).", needs_layer: true, schema: schema!({"type":"object","properties":{"exposure":{"type":"integer"},"offset":{"type":"integer"},"gamma":{"type":"integer"}},"required":["exposure","offset","gamma"],"additionalProperties":false}) },
    Command { name: "levels", description: "Levels: input black/white points 0..255, gamma in hundredths (100 neutral), output black/white 0..255.", needs_layer: true, schema: schema!({"type":"object","properties":{"input_black":{"type":"integer"},"input_white":{"type":"integer"},"gamma":{"type":"integer"},"output_black":{"type":"integer"},"output_white":{"type":"integer"}},"required":["input_black","input_white","gamma","output_black","output_white"],"additionalProperties":false}) },
    Command { name: "color_balance", description: "Colour balance: for shadows, midtones and highlights, three values -100..100 for cyan-red, magenta-green, yellow-blue.", needs_layer: true, schema: schema!({"type":"object","properties":{"shadows":{"type":"array","items":{"type":"integer"},"minItems":3,"maxItems":3},"midtones":{"type":"array","items":{"type":"integer"},"minItems":3,"maxItems":3},"highlights":{"type":"array","items":{"type":"integer"},"minItems":3,"maxItems":3}},"required":["shadows","midtones","highlights"],"additionalProperties":false}) },
    Command { name: "photo_filter", description: "A colour filter over the selected layer: an RGB colour (0..255 each) at a density percent 1..100. Warming: [236,138,0]; cooling: [0,109,232].", needs_layer: true, schema: schema!({"type":"object","properties":{"color":{"type":"array","items":{"type":"integer"},"minItems":3,"maxItems":3},"density":{"type":"integer"}},"required":["color","density"],"additionalProperties":false}) },
    Command { name: "black_and_white", description: "Convert the selected layer to black and white.", needs_layer: true, schema: no_args },
    Command { name: "threshold", description: "Threshold the selected layer to pure black and white at a level 1..255.", needs_layer: true, schema: schema!({"type":"object","properties":{"level":{"type":"integer"}},"required":["level"],"additionalProperties":false}) },
    Command { name: "posterize", description: "Posterize the selected layer to N tonal levels per channel (2..255).", needs_layer: true, schema: schema!({"type":"object","properties":{"levels":{"type":"integer"}},"required":["levels"],"additionalProperties":false}) },
    Command { name: "auto_tone", description: "Auto Tone: stretch each channel's histogram of the selected layer.", needs_layer: true, schema: no_args },
    Command { name: "auto_contrast", description: "Auto Contrast on the selected layer.", needs_layer: true, schema: no_args },
    Command { name: "auto_color", description: "Auto Color: neutralise a colour cast on the selected layer.", needs_layer: true, schema: no_args },
    Command { name: "gaussian_blur", description: "Gaussian blur of the selected layer by a radius in pixels (1..250).", needs_layer: true, schema: schema!({"type":"object","properties":{"radius":{"type":"integer"}},"required":["radius"],"additionalProperties":false}) },
    Command { name: "motion_blur", description: "Motion blur: angle in degrees and distance in pixels.", needs_layer: true, schema: schema!({"type":"object","properties":{"angle":{"type":"number"},"distance":{"type":"integer"}},"required":["angle","distance"],"additionalProperties":false}) },
    Command { name: "sharpen", description: "Sharpen the selected layer once.", needs_layer: true, schema: no_args },
    Command { name: "unsharp_mask", description: "Unsharp mask: radius in pixels, amount as a factor (0.1..5), threshold 0..255.", needs_layer: true, schema: schema!({"type":"object","properties":{"radius":{"type":"integer"},"amount":{"type":"number"},"threshold":{"type":"integer"}},"required":["radius","amount","threshold"],"additionalProperties":false}) },
    Command { name: "reduce_noise", description: "Reduce noise: strength 0..10 and preserve details 0..100.", needs_layer: true, schema: schema!({"type":"object","properties":{"strength":{"type":"integer"},"preserve_details":{"type":"integer"}},"required":["strength","preserve_details"],"additionalProperties":false}) },
    Command { name: "median", description: "Median filter by a radius in pixels (1..100).", needs_layer: true, schema: schema!({"type":"object","properties":{"radius":{"type":"integer"}},"required":["radius"],"additionalProperties":false}) },
    Command { name: "flip_layer_horizontal", description: "Mirror the selected layer left to right.", needs_layer: true, schema: no_args },
    Command { name: "flip_layer_vertical", description: "Flip the selected layer top to bottom.", needs_layer: true, schema: no_args },
    Command { name: "select_subject", description: "Select the main subject of the selected layer (tolerance 0..255, 32 is usual).", needs_layer: true, schema: schema!({"type":"object","properties":{"tolerance":{"type":"integer"}},"required":["tolerance"],"additionalProperties":false}) },
    Command { name: "remove_background", description: "Make everything but the subject of the selected layer transparent (tolerance 0..255).", needs_layer: true, schema: schema!({"type":"object","properties":{"tolerance":{"type":"integer"}},"required":["tolerance"],"additionalProperties":false}) },
    Command { name: "select_sky", description: "Select the sky on the selected layer.", needs_layer: true, schema: no_args },
    Command { name: "select_rectangle", description: "Select a rectangle in canvas pixels: x0, y0 to x1, y1.", needs_layer: false, schema: schema!({"type":"object","properties":{"x0":{"type":"number"},"y0":{"type":"number"},"x1":{"type":"number"},"y1":{"type":"number"}},"required":["x0","y0","x1","y1"],"additionalProperties":false}) },
    Command { name: "select_all", description: "Select the whole canvas.", needs_layer: false, schema: no_args },
    Command { name: "deselect", description: "Drop the selection.", needs_layer: false, schema: no_args },
    Command { name: "invert_selection", description: "Invert the selection.", needs_layer: false, schema: no_args },
    Command { name: "feather_selection", description: "Feather the selection by a radius in pixels.", needs_layer: false, schema: schema!({"type":"object","properties":{"radius":{"type":"integer"}},"required":["radius"],"additionalProperties":false}) },
    Command { name: "expand_selection", description: "Expand the selection by N pixels.", needs_layer: false, schema: schema!({"type":"object","properties":{"amount":{"type":"integer"}},"required":["amount"],"additionalProperties":false}) },
    Command { name: "contract_selection", description: "Contract the selection by N pixels.", needs_layer: false, schema: schema!({"type":"object","properties":{"amount":{"type":"integer"}},"required":["amount"],"additionalProperties":false}) },
    Command { name: "content_aware_fill", description: "Fill the selection from its surroundings (classical patch synthesis).", needs_layer: true, schema: no_args },
    Command { name: "generative_fill", description: "Fill the selection with the on-device generative model (no prompt; it hallucinates from the surroundings).", needs_layer: true, schema: no_args },
    Command { name: "prompt_to_edit", description: "Redraw the selection under a text prompt with the on-device diffusion model (landscapes: city, field, forest, lake, mountain, ocean, road). strength 0.05..1.", needs_layer: true, schema: schema!({"type":"object","properties":{"prompt":{"type":"string"},"seed":{"type":"integer"},"strength":{"type":"number"}},"required":["prompt","seed","strength"],"additionalProperties":false}) },
    Command { name: "generate_image", description: "Generate a new 192x192 landscape layer from a text prompt with the on-device diffusion model (it knows: city, field, forest, lake, mountain, ocean, road).", needs_layer: false, schema: schema!({"type":"object","properties":{"prompt":{"type":"string"},"seed":{"type":"integer"}},"required":["prompt","seed"],"additionalProperties":false}) },
    Command { name: "fill_selection", description: "Fill the selection on the selected layer with an RGBA colour (0..255 each).", needs_layer: true, schema: schema!({"type":"object","properties":{"color":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4}},"required":["color"],"additionalProperties":false}) },
    Command { name: "add_solid_color_layer", description: "Add a new layer filled with an RGBA colour.", needs_layer: false, schema: schema!({"type":"object","properties":{"color":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4}},"required":["color"],"additionalProperties":false}) },
    Command { name: "add_text_layer", description: "Add a text layer. `text` holds the type: the text, x/y position in pixels, size in pixels, RGBA colour, vertical flag, and font (\"Open Sans\" or null for the bitmap face).", needs_layer: false, schema: schema!({"type":"object","properties":{"name":{"type":"string"},"text":{"type":"object","properties":{"text":{"type":"string"},"x":{"type":"integer"},"y":{"type":"integer"},"size":{"type":"integer"},"color":{"type":"array","items":{"type":"integer"},"minItems":4,"maxItems":4},"vertical":{"type":"boolean"},"font":{"type":["string","null"]}},"required":["text","x","y","size","color","vertical","font"],"additionalProperties":false}},"required":["name","text"],"additionalProperties":false}) },
    Command { name: "set_layer_opacity", description: "Set the selected layer's opacity, 0..1.", needs_layer: true, schema: schema!({"type":"object","properties":{"opacity":{"type":"number"}},"required":["opacity"],"additionalProperties":false}) },
    Command { name: "duplicate_layer", description: "Duplicate the selected layer.", needs_layer: true, schema: no_args },
    Command { name: "remove_layer", description: "Delete the selected layer.", needs_layer: true, schema: no_args },
    Command { name: "merge_visible", description: "Merge all visible layers into one.", needs_layer: false, schema: no_args },
    Command { name: "flatten_image", description: "Flatten the whole document into one layer.", needs_layer: false, schema: no_args },
    Command { name: "resize_canvas", description: "Change the canvas size in pixels, anchoring the existing pixels at a reference point (topLeft, top, topRight, left, center, right, bottomLeft, bottom, bottomRight).", needs_layer: false, schema: schema!({"type":"object","properties":{"width":{"type":"integer"},"height":{"type":"integer"},"anchor":{"type":"string"}},"required":["width","height","anchor"],"additionalProperties":false}) },
    Command { name: "undo", description: "Undo the last edit.", needs_layer: false, schema: no_args },
    Command { name: "redo", description: "Redo the last undone edit.", needs_layer: false, schema: no_args },
];

pub fn command(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|c| c.name == name)
}

/// The tool definitions as the Messages API takes them.
pub fn tools() -> Vec<Value> {
    COMMANDS
        .iter()
        .map(|c| json!({ "name": c.name, "description": c.description, "input_schema": (c.schema)() }))
        .collect()
}

/// The names of the commands that act on the selected layer.
pub fn layer_commands() -> Vec<&'static str> {
    COMMANDS
        .iter()
        .filter(|c| c.needs_layer)
        .map(|c| c.name)
        .collect()
}

/// What the app tells the assistant about the open document.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DocumentSummary {
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub layers: Vec<LayerSummary>,
    #[serde(default)]
    pub selected_layer: Option<u32>,
    #[serde(default)]
    pub has_selection: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LayerSummary {
    pub id: u32,
    pub name: String,
    #[serde(default = "yes")]
    pub visible: bool,
}

fn yes() -> bool {
    true
}

pub fn system_prompt(document: &DocumentSummary) -> String {
    let mut s = String::from(
        "You are the AI Assisted Editor inside image-editor, a desktop image editor. \
         You edit the user's open document by calling the tools, which are the editor's own commands; \
         every call is applied immediately, undoably, to the document. Prefer one well-chosen call over \
         many; when a request is ambiguous, pick sensible Photoshop-like defaults and say what you chose. \
         Tools marked as acting on the selected layer act on the layer the user has selected. \
         Answer briefly, in plain language, and never claim an edit you did not make with a tool.\n\n",
    );
    if document.width == 0 || document.height == 0 {
        s.push_str("No document is open: only new_document-independent tools such as generate_image can create content.\n");
    } else {
        s.push_str(&format!(
            "Open document: {}x{} pixels, {} layer(s):\n",
            document.width,
            document.height,
            document.layers.len()
        ));
        for layer in &document.layers {
            s.push_str(&format!(
                "- layer {} \"{}\"{}{}\n",
                layer.id,
                layer.name,
                if layer.visible { "" } else { " (hidden)" },
                if document.selected_layer == Some(layer.id) {
                    " (selected)"
                } else {
                    ""
                }
            ));
        }
        s.push_str(if document.has_selection {
            "There is an active selection; selection-based tools act inside it.\n"
        } else {
            "There is no active selection.\n"
        });
    }
    s
}

/// One tool call the app should run: the command, its arguments, and
/// the tool-use id the result must be reported under.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Action {
    pub id: String,
    pub name: String,
    pub input: Value,
    pub needs_layer: bool,
}

/// What comes back to the app.
#[derive(Debug, Clone, Serialize)]
pub struct Reply {
    /// "claude" or "rules".
    pub mode: &'static str,
    pub model: String,
    pub text: String,
    pub actions: Vec<Action>,
    /// The assistant turn to append to the conversation before reporting
    /// the actions' results (the Messages API's own content blocks).
    pub content: Value,
    pub stop_reason: String,
}

/// Reads a Messages API response into a `Reply`.
pub fn reply_from_response(response: &Value) -> Result<Reply, String> {
    let content = response
        .get("content")
        .and_then(Value::as_array)
        .ok_or_else(|| "the model's response had no content".to_string())?;
    let mut text = String::new();
    let mut actions = Vec::new();
    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(Value::as_str) {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                let name = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let needs_layer = command(&name).is_some_and(|c| c.needs_layer);
                actions.push(Action {
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name,
                    input: block.get("input").cloned().unwrap_or(json!({})),
                    needs_layer,
                });
            }
            _ => {}
        }
    }
    let stop_reason = response
        .get("stop_reason")
        .and_then(Value::as_str)
        .unwrap_or("end_turn")
        .to_string();
    if stop_reason == "refusal" {
        text = "The model declined this request.".to_string();
        actions.clear();
    }
    Ok(Reply {
        mode: "claude",
        model: response
            .get("model")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_MODEL)
            .to_string(),
        text,
        actions,
        content: Value::Array(content.clone()),
        stop_reason,
    })
}

/// The request body for one turn.
pub fn request_body(model: &str, document: &DocumentSummary, messages: &Value) -> Value {
    json!({
        "model": model,
        "max_tokens": 8192,
        "fallbacks": "default",
        "system": [{ "type": "text", "text": system_prompt(document) }],
        "tools": tools(),
        "messages": messages,
    })
}

/// Asks Claude. `base_url` is Anthropic's API (or a stand-in under test).
pub fn ask_claude(
    base_url: &str,
    api_key: &str,
    model: &str,
    document: &DocumentSummary,
    messages: &Value,
) -> Result<Reply, String> {
    // The environment's proxy is for the real API; a loopback stand-in
    // (tests) is reached directly.
    let loopback =
        base_url.starts_with("http://127.0.0.1") || base_url.starts_with("http://localhost");
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(!loopback)
        .timeout(std::time::Duration::from_secs(300))
        .build();
    let body = request_body(model, document, messages);
    let response = agent
        .post(&format!("{}/v1/messages", base_url.trim_end_matches('/')))
        .set("x-api-key", api_key)
        .set("anthropic-version", "2023-06-01")
        .set("anthropic-beta", "server-side-fallback-2026-07-01")
        .set("content-type", "application/json")
        .send_string(&body.to_string());
    let response = match response {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let mut detail = String::new();
            let _ = r.into_reader().take(4096).read_to_string(&mut detail);
            let message = serde_json::from_str::<Value>(&detail)
                .ok()
                .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_string))
                .unwrap_or(detail);
            return Err(format!("Anthropic's API returned {code}: {message}"));
        }
        Err(e) => return Err(format!("could not reach Anthropic's API: {e}")),
    };
    let mut text = String::new();
    response
        .into_reader()
        .take(16 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("unreadable response: {e}"))?;
    reply_from_response(&value)
}

/// The rule-based reader: the plainest requests, with no model at all.
/// Returns the reply text and the actions to run.
pub fn rules(message: &str) -> Reply {
    let m = message.to_lowercase();
    let number = |default: i64| -> i64 {
        m.split(|c: char| !c.is_ascii_digit())
            .find(|s| !s.is_empty())
            .and_then(|s| s.parse().ok())
            .unwrap_or(default)
    };
    let has = |words: &[&str]| words.iter().any(|w| m.contains(w));
    let less = has(&["less", "lower", "reduce", "decrease", "darker", "dimmer"]);
    let mut actions: Vec<(&str, Value)> = Vec::new();
    let mut said: Vec<String> = Vec::new();
    if has(&["undo"]) {
        actions.push(("undo", json!({})));
        said.push("undoing the last edit".into());
    } else if has(&["redo"]) {
        actions.push(("redo", json!({})));
        said.push("redoing".into());
    }
    if let Some(rest) = m
        .split("generate ")
        .nth(1)
        .or_else(|| m.split("draw ").nth(1))
    {
        let prompt = rest
            .trim_start_matches("an image of ")
            .trim_start_matches("a picture of ")
            .trim_start_matches("a ")
            .trim();
        if !prompt.is_empty() {
            actions.push(("generate_image", json!({ "prompt": prompt, "seed": 1 })));
            said.push(format!("generating \"{prompt}\" as a new layer"));
        }
    }
    if has(&["bright", "darker", "dim"]) {
        let amount = if has(&["darker", "dim"]) || less {
            -20
        } else {
            20
        };
        actions.push((
            "brightness_contrast",
            json!({ "brightness": amount, "contrast": 0 }),
        ));
        said.push(format!("brightness {amount:+}"));
    }
    if has(&["contrast"]) {
        let amount = if less { -20 } else { 20 };
        actions.push((
            "brightness_contrast",
            json!({ "brightness": 0, "contrast": amount }),
        ));
        said.push(format!("contrast {amount:+}"));
    }
    if has(&["saturat", "vivid", "colourful", "colorful"]) {
        let amount = if less || has(&["desaturat"]) { -25 } else { 25 };
        actions.push((
            "hue_saturation",
            json!({ "hue": 0, "saturation": amount, "lightness": 0 }),
        ));
        said.push(format!("saturation {amount:+}"));
    }
    if has(&["black and white", "grayscale", "greyscale", "monochrome"]) {
        actions.push(("black_and_white", json!({})));
        said.push("black and white".into());
    }
    if has(&["blur"]) {
        let radius = number(4).clamp(1, 250);
        actions.push(("gaussian_blur", json!({ "radius": radius })));
        said.push(format!("Gaussian blur {radius} px"));
    }
    if has(&["sharpen", "sharper", "crisper"]) {
        actions.push(("sharpen", json!({})));
        said.push("sharpen".into());
    }
    if has(&["noise", "grain", "denoise"]) && !has(&["add noise", "add grain"]) {
        actions.push((
            "reduce_noise",
            json!({ "strength": 5, "preserve_details": 50 }),
        ));
        said.push("reduce noise".into());
    }
    if has(&["warm"]) {
        actions.push((
            "photo_filter",
            json!({ "color": [236, 138, 0], "density": 25 }),
        ));
        said.push("a warming filter".into());
    } else if has(&["cool", "colder"]) {
        actions.push((
            "photo_filter",
            json!({ "color": [0, 109, 232], "density": 25 }),
        ));
        said.push("a cooling filter".into());
    }
    if has(&[
        "auto tone",
        "fix the exposure",
        "fix exposure",
        "auto levels",
    ]) {
        actions.push(("auto_tone", json!({})));
        said.push("Auto Tone".into());
    }
    if has(&["auto color", "auto colour", "colour cast", "color cast"]) {
        actions.push(("auto_color", json!({})));
        said.push("Auto Color".into());
    }
    if has(&["remove the background", "remove background", "cut out"]) {
        actions.push(("remove_background", json!({ "tolerance": 32 })));
        said.push("removing the background".into());
    } else if has(&["select the subject", "select subject"]) {
        actions.push(("select_subject", json!({ "tolerance": 32 })));
        said.push("selecting the subject".into());
    } else if has(&["select the sky", "select sky"]) {
        actions.push(("select_sky", json!({})));
        said.push("selecting the sky".into());
    } else if has(&["select all", "select everything"]) {
        actions.push(("select_all", json!({})));
        said.push("selecting all".into());
    } else if has(&["deselect", "drop the selection"]) {
        actions.push(("deselect", json!({})));
        said.push("deselecting".into());
    }
    if has(&["invert the selection", "invert selection"]) {
        actions.push(("invert_selection", json!({})));
        said.push("inverting the selection".into());
    }
    if has(&[
        "mirror",
        "flip horizontal",
        "flip it horizontal",
        "flip left",
    ]) {
        actions.push(("flip_layer_horizontal", json!({})));
        said.push("flipping horizontally".into());
    } else if has(&["flip vertical", "flip it vertical", "upside down"]) {
        actions.push(("flip_layer_vertical", json!({})));
        said.push("flipping vertically".into());
    }
    if has(&["fill the selection", "generative fill", "fill it in"]) {
        actions.push(("generative_fill", json!({})));
        said.push("filling the selection with the generative model".into());
    }
    let text = if actions.is_empty() {
        "Without an Anthropic API key (External Services) I only follow plain requests: brighter or darker, more or less contrast or saturation, black and white, blur, sharpen, reduce noise, warmer or cooler, auto tone, auto color, select the subject or the sky, remove the background, flip, undo, or \"generate <a landscape>\".".to_string()
    } else {
        format!("Applying: {}.", said.join(", "))
    };
    Reply {
        mode: "rules",
        model: "rules".into(),
        text,
        actions: actions
            .into_iter()
            .enumerate()
            .map(|(i, (name, input))| Action {
                id: format!("rule_{i}"),
                name: name.into(),
                input,
                needs_layer: command(name).is_some_and(|c| c.needs_layer),
            })
            .collect(),
        content: Value::Null,
        stop_reason: "end_turn".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_well_formed() {
        let names: Vec<&str> = COMMANDS.iter().map(|c| c.name).collect();
        let mut unique = names.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(names.len(), unique.len(), "unique names");
        for tool in tools() {
            assert_eq!(tool["input_schema"]["type"], "object", "{}", tool["name"]);
            assert_eq!(
                tool["input_schema"]["additionalProperties"], false,
                "{}",
                tool["name"]
            );
            assert!(!tool["description"].as_str().unwrap().is_empty());
        }
        assert!(layer_commands().contains(&"gaussian_blur"));
        assert!(!layer_commands().contains(&"select_all"));
        assert!(names.len() > 40);
    }

    #[test]
    fn the_system_prompt_describes_the_document() {
        let doc = DocumentSummary {
            width: 640,
            height: 480,
            layers: vec![
                LayerSummary {
                    id: 1,
                    name: "Background".into(),
                    visible: true,
                },
                LayerSummary {
                    id: 2,
                    name: "Type".into(),
                    visible: false,
                },
            ],
            selected_layer: Some(2),
            has_selection: true,
        };
        let prompt = system_prompt(&doc);
        assert!(prompt.contains("640x480"));
        assert!(prompt.contains("layer 2 \"Type\" (hidden) (selected)"));
        assert!(prompt.contains("active selection"));
        assert!(system_prompt(&DocumentSummary::default()).contains("No document is open"));
        let body = request_body(
            DEFAULT_MODEL,
            &doc,
            &json!([{ "role": "user", "content": "hi" }]),
        );
        assert_eq!(body["model"], DEFAULT_MODEL);
        assert_eq!(body["fallbacks"], "default");
        assert!(body["tools"].as_array().unwrap().len() > 40);
        assert!(
            body.get("thinking").is_none(),
            "adaptive by default on this model"
        );
    }

    #[test]
    fn a_response_becomes_text_and_actions() {
        let response = json!({
            "model": "claude-opus-5",
            "stop_reason": "tool_use",
            "content": [
                { "type": "text", "text": "I'll brighten it a little." },
                { "type": "tool_use", "id": "toolu_1", "name": "brightness_contrast", "input": { "brightness": 20, "contrast": 0 } },
                { "type": "tool_use", "id": "toolu_2", "name": "select_all", "input": {} }
            ]
        });
        let reply = reply_from_response(&response).unwrap();
        assert_eq!(reply.text, "I'll brighten it a little.");
        assert_eq!(reply.actions.len(), 2);
        assert_eq!(reply.actions[0].name, "brightness_contrast");
        assert!(reply.actions[0].needs_layer);
        assert!(!reply.actions[1].needs_layer);
        assert_eq!(reply.stop_reason, "tool_use");
        assert_eq!(reply.content.as_array().unwrap().len(), 3);
        let refused = reply_from_response(
            &json!({ "content": [{ "type": "text", "text": "x" }], "stop_reason": "refusal" }),
        )
        .unwrap();
        assert!(refused.actions.is_empty());
        assert!(refused.text.contains("declined"));
        assert!(reply_from_response(&json!({ "error": "x" })).is_err());
    }

    #[test]
    fn the_rules_read_plain_requests() {
        let r = rules("Make it a bit brighter and sharper");
        let names: Vec<&str> = r.actions.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["brightness_contrast", "sharpen"]);
        assert_eq!(r.actions[0].input["brightness"], 20);
        assert_eq!(r.mode, "rules");
        let r = rules("darker, less contrast");
        assert_eq!(r.actions[0].input["brightness"], -20);
        assert_eq!(r.actions[1].input["contrast"], -20);
        let r = rules("blur it by 12 pixels");
        assert_eq!(r.actions[0].input["radius"], 12);
        let r = rules("Select the subject and remove the background");
        assert_eq!(
            r.actions
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>(),
            vec!["remove_background"]
        );
        let r = rules("generate a misty lake at dawn");
        assert_eq!(r.actions[0].name, "generate_image");
        assert_eq!(r.actions[0].input["prompt"], "misty lake at dawn");
        assert!(!r.actions[0].needs_layer);
        let r = rules("what is the meaning of life");
        assert!(r.actions.is_empty());
        assert!(r.text.contains("API key"));
        assert_eq!(rules("mirror it").actions[0].name, "flip_layer_horizontal");
        assert_eq!(
            rules("make it warmer").actions[0].input["color"],
            json!([236, 138, 0])
        );
    }
}
