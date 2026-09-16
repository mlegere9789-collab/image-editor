"""
A small, real, fully-convolutional photo-restoration network -- trained
from scratch for this project, not a pretrained checkpoint. Same
encoder-decoder shape as TinyColorizer, widened to full RGB in and out
(restoration needs to correct all three channels, not just chrominance).

Input: a real, synthetically degraded RGB image, 3xHxW, 0..1.
Output: the network's real prediction of the clean RGB image, 3xHxW,
0..1 (via sigmoid).
"""

import torch
from torch import nn


class TinyRestorer(nn.Module):
    def __init__(self):
        super().__init__()
        self.enc1 = nn.Sequential(nn.Conv2d(3, 32, 3, padding=1), nn.ReLU(inplace=True))
        self.enc2 = nn.Sequential(nn.Conv2d(32, 64, 3, stride=2, padding=1), nn.ReLU(inplace=True))
        self.enc3 = nn.Sequential(nn.Conv2d(64, 128, 3, stride=2, padding=1), nn.ReLU(inplace=True))
        self.bottleneck = nn.Sequential(
            nn.Conv2d(128, 128, 3, padding=1),
            nn.ReLU(inplace=True),
            nn.Conv2d(128, 128, 3, padding=1),
            nn.ReLU(inplace=True),
        )
        self.dec2 = nn.Sequential(nn.Conv2d(128, 64, 3, padding=1), nn.ReLU(inplace=True))
        self.dec1 = nn.Sequential(nn.Conv2d(64, 32, 3, padding=1), nn.ReLU(inplace=True))
        self.out = nn.Conv2d(32, 3, 3, padding=1)

    def forward(self, x):
        e1 = self.enc1(x)
        e2 = self.enc2(e1)
        e3 = self.enc3(e2)
        b = self.bottleneck(e3)
        u2 = nn.functional.interpolate(b, scale_factor=2, mode="nearest")
        d2 = self.dec2(u2) + e2
        u1 = nn.functional.interpolate(d2, scale_factor=2, mode="nearest")
        d1 = self.dec1(u1) + e1
        residual = torch.tanh(self.out(d1))
        # Predict a correction on top of the degraded input rather than
        # the clean image outright -- residual learning, standard in
        # the real image-restoration literature (e.g. DnCNN), and an
        # easier real optimization target than reconstructing every
        # pixel from nothing.
        return torch.clamp(x + residual, 0.0, 1.0)
