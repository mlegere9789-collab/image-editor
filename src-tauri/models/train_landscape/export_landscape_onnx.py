"""
Exports the model this project just trained itself (style_net.pt --
its own state_dict, saved by train_landscape.py moments ago, not a
downloaded checkpoint) to ONNX at opset 13.
"""

import torch

from transform_net import TransformNet

model = TransformNet()
state = torch.load("style_net.pt", map_location="cpu", weights_only=True)
model.load_state_dict(state)
model.eval()

dummy = torch.zeros(1, 3, 128, 128, dtype=torch.float32)
with torch.no_grad():
    ref = model(dummy)
print("reference output shape:", tuple(ref.shape))

torch.onnx.export(
    model,
    dummy,
    "landscape_mixer.onnx",
    input_names=["content"],
    output_names=["mixed"],
    opset_version=13,
    do_constant_folding=True,
    dynamic_axes={"content": {2: "height", 3: "width"}, "mixed": {2: "height", 3: "width"}},
)
print("exported to landscape_mixer.onnx")
