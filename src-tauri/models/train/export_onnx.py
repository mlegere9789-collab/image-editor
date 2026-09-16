"""
Exports the model this project just trained itself (tiny_colorizer.pt --
its own state_dict, saved by train.py moments ago, not a downloaded
checkpoint) to ONNX at opset 13.
"""

import torch

from model import TinyColorizer

model = TinyColorizer()
state = torch.load("tiny_colorizer.pt", map_location="cpu", weights_only=True)
model.load_state_dict(state)
model.eval()

dummy = torch.zeros(1, 1, 96, 96, dtype=torch.float32)
with torch.no_grad():
    ref = model(dummy)
print("reference output shape:", tuple(ref.shape))

torch.onnx.export(
    model,
    dummy,
    "tiny_colorizer.onnx",
    input_names=["y"],
    output_names=["cbcr"],
    opset_version=13,
    do_constant_folding=True,
    dynamic_axes={"y": {2: "height", 3: "width"}, "cbcr": {2: "height", 3: "width"}},
)
print("exported to tiny_colorizer.onnx")
