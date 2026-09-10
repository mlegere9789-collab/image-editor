"""
Exports the model this project just trained itself (tiny_restorer.pt --
its own state_dict, not a downloaded checkpoint) to ONNX at opset 13.
"""

import torch

from model import TinyRestorer

model = TinyRestorer()
state = torch.load("tiny_restorer.pt", map_location="cpu", weights_only=True)
model.load_state_dict(state)
model.eval()

dummy = torch.zeros(1, 3, 96, 96, dtype=torch.float32)
with torch.no_grad():
    ref = model(dummy)
print("reference output shape:", tuple(ref.shape))

torch.onnx.export(
    model,
    dummy,
    "tiny_restorer.onnx",
    input_names=["degraded"],
    output_names=["restored"],
    opset_version=13,
    do_constant_folding=True,
    dynamic_axes={"degraded": {2: "height", 3: "width"}, "restored": {2: "height", 3: "width"}},
)
print("exported to tiny_restorer.onnx")
