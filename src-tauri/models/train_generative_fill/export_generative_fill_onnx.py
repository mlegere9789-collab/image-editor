"""
Exports the model this project just trained itself (inpaint_net.pt --
its own state_dict, saved by train_generative_fill.py moments ago, not a
downloaded checkpoint) to ONNX at opset 13.

Fixed 128x128 input/output (no dynamic height/width axes), matching the
one size this network was ever trained on -- two real reasons, not one:
the skip connections' own `Concat` nodes need shapes `tract` (this
project's Rust inference engine) can actually prove match at graph-load
time, which its shape inference cannot always do symbolically across a
Resize-then-Concat under dynamic axes (confirmed directly: dynamic axes
here failed to load in tract with an InferenceConcat analysis error);
and running the network at some other size would itself be a real
distribution shift this small, fixed-receptive-field architecture has
no proven robustness to, on top of the shape problem. `generative_fill.rs`
resizes its own context-window crop to exactly 128x128 before inference
and the masked prediction back to the crop's real size afterward, so
every crop the model ever sees matches training exactly.
"""

import torch

from inpaint_net import InpaintNet

model = InpaintNet(in_channels=5)
state = torch.load("inpaint_net_seeded.pt", map_location="cpu", weights_only=True)
model.load_state_dict(state)
model.eval()

# 5 channels: RGB (hole zeroed), a 1-channel hole mask, and the seed's
# per-pixel noise plane (train_similar.py's fine-tune).
dummy = torch.zeros(1, 5, 128, 128, dtype=torch.float32)
with torch.no_grad():
    ref = model(dummy)
print("reference output shape:", tuple(ref.shape))

torch.onnx.export(
    model,
    dummy,
    "generative_fill.onnx",
    input_names=["masked_rgb_mask_and_noise"],
    output_names=["filled"],
    opset_version=13,
    do_constant_folding=True,
    dynamo=False,
)
print("exported to generative_fill.onnx")
