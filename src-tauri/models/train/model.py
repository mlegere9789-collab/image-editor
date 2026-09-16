"""
A small, real, fully-convolutional colorization network -- designed and
trained from scratch for this project, not a pretrained checkpoint.

Input: the Y (luma) channel, 1xHxW, 0..1.
Output: Cb, Cr (chrominance), 2xHxW, centered on 0 (i.e. -0.5..0.5).

Kept deliberately small (encoder-decoder, ~40K params) for CPU training
in a reasonable time on a real but small (51-image) dataset. Upsampling
uses nearest-neighbour + conv (exports to ONNX's `Resize` op, which
`tract` -- this project's Rust inference engine -- implements; the
deprecated `Upsample` op it does not, learned the hard way earlier this
same investigation).
"""

import torch
from torch import nn


class TinyColorizer(nn.Module):
    def __init__(self):
        super().__init__()
        self.enc1 = nn.Sequential(nn.Conv2d(1, 16, 3, padding=1), nn.ReLU(inplace=True))
        self.enc2 = nn.Sequential(nn.Conv2d(16, 32, 3, stride=2, padding=1), nn.ReLU(inplace=True))
        self.enc3 = nn.Sequential(nn.Conv2d(32, 64, 3, stride=2, padding=1), nn.ReLU(inplace=True))
        self.bottleneck = nn.Sequential(nn.Conv2d(64, 64, 3, padding=1), nn.ReLU(inplace=True))
        self.dec2 = nn.Sequential(nn.Conv2d(64, 32, 3, padding=1), nn.ReLU(inplace=True))
        self.dec1 = nn.Sequential(nn.Conv2d(32, 16, 3, padding=1), nn.ReLU(inplace=True))
        self.out = nn.Conv2d(16, 2, 3, padding=1)

    def forward(self, y):
        e1 = self.enc1(y)
        e2 = self.enc2(e1)
        e3 = self.enc3(e2)
        b = self.bottleneck(e3)
        u2 = nn.functional.interpolate(b, scale_factor=2, mode="nearest")
        d2 = self.dec2(u2) + e2
        u1 = nn.functional.interpolate(d2, scale_factor=2, mode="nearest")
        d1 = self.dec1(u1) + e1
        out = torch.tanh(self.out(d1)) * 0.5
        return out
