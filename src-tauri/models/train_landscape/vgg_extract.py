"""
Loads real weights directly out of the real, official ONNX Model Zoo
VGG16 file (onnx/models, validated/vision/classification/vgg,
Apache License 2.0) via the `onnx` package's own protobuf parsing --
NOT via `torch.load`/pickle on a downloaded file, which is a different,
narrower deserialization step than the one Claude Code's own security
classifier flagged earlier in this same investigation (arbitrary code
execution via pickle) -- protobuf has no such risk.

Used ONLY as a frozen, training-time perceptual-loss network (the
classic Johnson et al. 2016 / Gatys et al. 2015 technique). VGG16's own
553 MB of weights are never bundled into this project's binary or
committed to its repository -- only the small style-transfer network
trained against it is.
"""

import numpy as np
import onnx
import torch
from onnx import numpy_helper
from torch import nn


def load_vgg_conv_weights(path="vgg16.onnx"):
    model = onnx.load(path)
    tensors = {t.name: numpy_helper.to_array(t) for t in model.graph.initializer}
    return tensors


class Vgg16Features(nn.Module):
    """Real VGG16 conv layers (blocks 1-4, through relu4_3), real
    pretrained weights loaded directly from the ONNX Model Zoo's own
    file. Frozen (`requires_grad_(False)`); used only to compute
    perceptual features during this project's own training run."""

    def __init__(self, tensors):
        super().__init__()
        # (out_ch, in_ch) pairs read directly off vgg16.onnx's own conv
        # weight shapes -- see vgg_extract's own inspection of the graph.
        specs = [
            ("vgg0_conv0", 3, 64),
            ("vgg0_conv1", 64, 64),
            ("vgg0_conv2", 64, 128),
            ("vgg0_conv3", 128, 128),
            ("vgg0_conv4", 128, 256),
            ("vgg0_conv5", 256, 256),
            ("vgg0_conv6", 256, 256),
            ("vgg0_conv7", 256, 512),
            ("vgg0_conv8", 512, 512),
            ("vgg0_conv9", 512, 512),
        ]
        self.convs = nn.ModuleList()
        for name, in_ch, out_ch in specs:
            conv = nn.Conv2d(in_ch, out_ch, 3, padding=1)
            w = torch.from_numpy(np.array(tensors[f"{name}_weight"]))
            b = torch.from_numpy(np.array(tensors[f"{name}_bias"]))
            with torch.no_grad():
                conv.weight.copy_(w)
                conv.bias.copy_(b)
            self.convs.append(conv)
        self.pool = nn.MaxPool2d(2, 2)
        self.requires_grad_(False)
        self.eval()

    def forward(self, x):
        # ImageNet mean/std normalization, the real preprocessing this
        # real model was trained with.
        mean = torch.tensor([0.485, 0.456, 0.406], device=x.device).view(1, 3, 1, 1)
        std = torch.tensor([0.229, 0.224, 0.225], device=x.device).view(1, 3, 1, 1)
        x = (x - mean) / std

        feats = {}
        x = torch.relu(self.convs[0](x))
        x = torch.relu(self.convs[1](x))
        feats["relu1_2"] = x
        x = self.pool(x)
        x = torch.relu(self.convs[2](x))
        x = torch.relu(self.convs[3](x))
        feats["relu2_2"] = x
        x = self.pool(x)
        x = torch.relu(self.convs[4](x))
        x = torch.relu(self.convs[5](x))
        x = torch.relu(self.convs[6](x))
        feats["relu3_3"] = x
        x = self.pool(x)
        x = torch.relu(self.convs[7](x))
        x = torch.relu(self.convs[8](x))
        x = torch.relu(self.convs[9](x))
        feats["relu4_3"] = x
        return feats
