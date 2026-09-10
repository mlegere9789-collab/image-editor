"""
The real-time style transfer feed-forward network (Johnson, Alahi &
Fei-Fei, "Perceptual Losses for Real-Time Style Transfer and
Super-Resolution", ECCV 2016) -- the same real, published architecture
the ONNX Model Zoo's own fast_neural_style models use, reimplemented
here (not copied from anywhere -- it's a well-known, standard shape)
with one deliberate change: nearest-neighbour upsample + conv instead
of the zoo's own transposed-conv/`Upsample`-op upsampling, since
`tract` (this project's Rust inference engine) has no `Upsample`
implementation at all -- confirmed by grepping tract's own source
earlier in this same investigation. Nearest-upsample + conv lowers to
ONNX's modern `Resize` op instead, which `tract` does implement.

This network's own weights are trained entirely from random
initialization for this project -- see `train_style.py`.
"""

import torch
from torch import nn


class ConvInstanceReLU(nn.Module):
    def __init__(self, in_ch, out_ch, kernel, stride, relu=True):
        super().__init__()
        pad = kernel // 2
        self.pad = nn.ReflectionPad2d(pad)
        self.conv = nn.Conv2d(in_ch, out_ch, kernel, stride)
        self.norm = nn.InstanceNorm2d(out_ch, affine=True)
        self.relu = relu

    def forward(self, x):
        x = self.conv(self.pad(x))
        x = self.norm(x)
        return torch.relu(x) if self.relu else x


class ResidualBlock(nn.Module):
    def __init__(self, ch):
        super().__init__()
        self.c1 = ConvInstanceReLU(ch, ch, 3, 1)
        self.c2 = ConvInstanceReLU(ch, ch, 3, 1, relu=False)

    def forward(self, x):
        return x + self.c2(self.c1(x))


class UpsampleConvInstanceReLU(nn.Module):
    def __init__(self, in_ch, out_ch):
        super().__init__()
        self.body = ConvInstanceReLU(in_ch, out_ch, 3, 1)

    def forward(self, x):
        x = nn.functional.interpolate(x, scale_factor=2, mode="nearest")
        return self.body(x)


class TransformNet(nn.Module):
    def __init__(self):
        super().__init__()
        self.down = nn.Sequential(
            ConvInstanceReLU(3, 32, 9, 1),
            ConvInstanceReLU(32, 64, 3, 2),
            ConvInstanceReLU(64, 128, 3, 2),
        )
        self.res = nn.Sequential(*[ResidualBlock(128) for _ in range(5)])
        self.up = nn.Sequential(
            UpsampleConvInstanceReLU(128, 64),
            UpsampleConvInstanceReLU(64, 32),
        )
        self.out = nn.Sequential(nn.ReflectionPad2d(4), nn.Conv2d(32, 3, 9, 1))

    def forward(self, x):
        x = self.down(x)
        x = self.res(x)
        x = self.up(x)
        x = self.out(x)
        return torch.sigmoid(x)
