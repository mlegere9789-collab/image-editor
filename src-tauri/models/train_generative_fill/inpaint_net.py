"""
A real, self-supervised generative-fill network: a context-encoder-style
architecture (Pathak, Krahenbuhl, Donahue, Darrell & Efros, "Context
Encoders: Feature Learning by Inpainting", CVPR 2016) with U-Net-style
skip connections (Ronneberger, Fischer & Brox, "U-Net: Convolutional
Networks for Biomedical Image Segmentation", MICCAI 2015) -- both real,
well-established, standard architectures, reimplemented here (not
copied from anywhere). Reuses this project's own `TransformNet`-style
building blocks (`train_style/transform_net.py`) so it lowers through
the exact ONNX ops `tract` (this project's Rust inference engine)
already proved it can run: reflection-padded convolutions, InstanceNorm,
and nearest-upsample + conv instead of a transposed convolution or the
`Upsample` op `tract` doesn't implement.

Why skip connections were added after the first architecture (a pure
context-encoder bottleneck, no skips) shipped a real but qualitatively
poor result: a small, uniform-colour hole (e.g. a patch of clear sky)
surrounded by strongly-coloured real context (e.g. a warm sunset) still
came back a flat, wrong-coloured patch -- the three stride-2 downsamples
compress the whole 128x128 input to a 16x16 bottleneck before the
decoder ever sees it, which is fine for the *semantic* "what kind of
scene is this" signal but genuinely loses the fine, local "this exact
pixel's neighbour is this exact shade of orange" signal a good fill
needs, especially in low-texture regions with few other cues. Skip
connections let the decoder read each encoder stage's own
higher-resolution features directly, carrying that local colour
information back in without forcing it through the bottleneck. The one
place this needs care versus a plain segmentation U-Net: the input
itself has the hole zeroed out, so a skip connection's own features
*inside* the hole are also near-zero -- not a source of leaked
"answer," just an absence of signal there, exactly like every other
input channel at that location. Outside the hole, the skip carries the
real thing.

This network's own weights are trained entirely from random
initialization for this project -- see `train_generative_fill.py`. No
pretrained checkpoint of any kind is loaded anywhere.
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
    """Nearest-upsample by 2, then a conv over the upsampled map
    concatenated with `skip` (the matching encoder stage's own
    higher-resolution features) along the channel axis -- the standard
    U-Net skip-connection shape."""

    def __init__(self, in_ch, skip_ch, out_ch):
        super().__init__()
        self.body = ConvInstanceReLU(in_ch + skip_ch, out_ch, 3, 1)

    def forward(self, x, skip):
        x = nn.functional.interpolate(x, scale_factor=2, mode="nearest")
        x = torch.cat([x, skip], dim=1)
        return self.body(x)


class InpaintNet(nn.Module):
    """`in_channels` in (4 = RGB with the hole zeroed + hole mask, the
    original model; 5 = those plus one channel of per-pixel Gaussian
    noise, the seeded model `train_similar.py` fine-tunes so Generate
    Similar can draw a different plausible fill per seed), 3-channel
    (RGB) out. The caller (`generative_fill.rs`) is responsible for
    compositing: keeping every known pixel exactly as it was and taking
    this network's prediction only inside the masked region, so a bug or
    a low-confidence guess here can never corrupt pixels outside the
    fill target."""

    def __init__(self, in_channels=4):
        super().__init__()
        self.down1 = ConvInstanceReLU(in_channels, 32, 9, 1)
        self.down2 = ConvInstanceReLU(32, 64, 3, 2)
        self.down3 = ConvInstanceReLU(64, 128, 3, 2)
        self.down4 = ConvInstanceReLU(128, 256, 3, 2)
        self.res = nn.Sequential(*[ResidualBlock(256) for _ in range(6)])
        self.up3 = UpsampleConvInstanceReLU(256, 128, 128)
        self.up2 = UpsampleConvInstanceReLU(128, 64, 64)
        self.up1 = UpsampleConvInstanceReLU(64, 32, 32)
        self.out = nn.Sequential(nn.ReflectionPad2d(4), nn.Conv2d(32, 3, 9, 1))

    def forward(self, x):
        s1 = self.down1(x)  # 32ch,  full res
        s2 = self.down2(s1)  # 64ch,  1/2
        s3 = self.down3(s2)  # 128ch, 1/4
        x = self.down4(s3)  # 256ch, 1/8
        x = self.res(x)
        x = self.up3(x, s3)  # 128ch, 1/4
        x = self.up2(x, s2)  # 64ch,  1/2
        x = self.up1(x, s1)  # 32ch,  full res
        x = self.out(x)
        return torch.sigmoid(x)
