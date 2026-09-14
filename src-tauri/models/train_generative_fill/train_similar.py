"""
Fine-tunes the shipped Generative Fill network into a SEEDED one for
Generate Similar: the same architecture with a fifth input channel of
per-pixel Gaussian noise, trained so that a different noise draw gives a
different plausible fill of the same hole.

Why plain "add a noise channel" is not enough, and what this does about
it: a network trained only with reconstruction losses (L1, perceptual)
learns to ignore any noise input, because the loss-optimal output for a
given context is one fixed answer (the conditional mean) whatever the
noise says -- so two seeds would come back byte-for-byte identical and
"Generate Similar" would be a lie. This run adds a mode-seeking
regularizer (Mao, Lee, Tseng, Ma & Yang, "Mode Seeking Generative
Adversarial Networks for Diverse Image Synthesis", CVPR 2019 -- the
ratio term of it, which is loss-agnostic; no discriminator is involved
here): two noise draws z1, z2 are run through the network on the same
context and the loss rewards output distance inside the hole in
proportion to the noise distance, so the network is pushed to actually
use the noise. The reconstruction terms keep every sample a plausible
fill; the mode-seeking term keeps the samples apart. `diversity` in the
log is the mean absolute difference between the two samples inside the
hole, in 0-255 units -- the number that says whether the feature is
real: near 0 means the noise is being ignored.

Warm-started from the shipped model's own weights, read straight out of
`../generative_fill.onnx` with the `onnx` package's protobuf parsing
(the same mechanism `vgg_extract.py` uses; no `torch.load` of any
downloaded file). The first convolution is widened from 4 to 5 input
channels with the new channel's weights zero-initialised, so at step 0
the network is exactly the shipped one and the noise has no effect yet;
the fine-tune then teaches it to respond. No checkpoint of any kind is
loaded from anywhere but this project's own exported model (or, with
`--resume`, this script's own previous checkpoint).

The weight on the mode-seeking term was tuned against a real fidelity
measurement, not guessed: at 0.02, four epochs held diversity at ~14/255
but hole-region L1 against ground truth on four held-out holes drifted
from the shipped model's 15.0 to 17.8 (0-255 units) and was still
rising, so the term was winning over reconstruction. Halved to 0.01 and
resumed from that epoch-4 checkpoint; the goal is diversity that is
visible while fidelity returns to within a few percent of the shipped
model's. `fidelity_check.py` is the measurement.
"""

import glob
import os
import random
import sys

import numpy as np
import onnx
import torch
from onnx import numpy_helper
from PIL import Image
from torch import nn

from inpaint_net import InpaintNet
from vgg_extract import Vgg16Features, load_vgg_conv_weights

PATCH = 128
BATCH = 4
STEPS_PER_EPOCH = 200
EPOCHS = 20
LR = 1e-4
HOLE_WEIGHT = 3.0
CONTEXT_WEIGHT = 0.3
PERCEPTUAL_WEIGHT = 0.08
MODE_SEEKING_WEIGHT = 0.01
MODE_SEEKING_EPS = 0.02
TV_WEIGHT = 1e-6
HOLE_MIN = 32
HOLE_MAX = 64
CHECKPOINT_EVERY = 2
SHIPPED_ONNX = "../generative_fill.onnx"


def load_dataset():
    images = []
    for path in sorted(glob.glob("raw/*")):
        im = Image.open(path).convert("RGB")
        if im.size[0] < PATCH or im.size[1] < PATCH:
            continue
        images.append(im)
    print(f"Loaded {len(images)} real RGB training images")
    return images


def random_patch(im):
    w, h = im.size
    x = random.randint(0, w - PATCH)
    y = random.randint(0, h - PATCH)
    patch = im.crop((x, y, x + PATCH, y + PATCH))
    if random.random() < 0.5:
        patch = patch.transpose(Image.FLIP_LEFT_RIGHT)
    return torch.from_numpy(np.array(patch, dtype="float32") / 255.0).permute(2, 0, 1)


def random_mask():
    hw = random.randint(HOLE_MIN, HOLE_MAX)
    hh = random.randint(HOLE_MIN, HOLE_MAX)
    x0 = random.randint(0, PATCH - hw)
    y0 = random.randint(0, PATCH - hh)
    mask = torch.zeros(1, PATCH, PATCH)
    mask[:, y0 : y0 + hh, x0 : x0 + hw] = 1.0
    return mask


def total_variation(img):
    return (
        (img[:, :, 1:, :] - img[:, :, :-1, :]).abs().mean()
        + (img[:, :, :, 1:] - img[:, :, :, :-1]).abs().mean()
    )


def shipped_state_dict(path):
    """The shipped 4-channel model's weights, keyed exactly as
    InpaintNet's own state_dict (the ONNX exporter kept the module
    paths as initializer names -- verified before this was written)."""
    model = onnx.load(path)
    return {t.name: torch.from_numpy(np.array(numpy_helper.to_array(t))) for t in model.graph.initializer}


def widened_from_shipped(path):
    net = InpaintNet(in_channels=5)
    shipped = shipped_state_dict(path)
    own = net.state_dict()
    missing = [k for k in own if k not in shipped]
    if missing:
        raise RuntimeError(f"shipped model is missing {missing}")
    for key, value in shipped.items():
        if key == "down1.conv.weight":
            widened = torch.zeros_like(own[key])
            widened[:, :4] = value
            own[key] = widened
        else:
            own[key] = value
    net.load_state_dict(own)
    return net


def save_checkpoint_atomic(net, path):
    tmp = path + ".tmp"
    torch.save(net.state_dict(), tmp)
    os.replace(tmp, path)


def main():
    torch.manual_seed(2)
    random.seed(2)
    images = load_dataset()

    vgg = Vgg16Features(load_vgg_conv_weights("vgg16.onnx"))
    if "--resume" in sys.argv:
        net = InpaintNet(in_channels=5)
        net.load_state_dict(
            torch.load("inpaint_net_seeded.pt", map_location="cpu", weights_only=True)
        )
        print("resumed from inpaint_net_seeded.pt")
    else:
        net = widened_from_shipped(SHIPPED_ONNX)
        print(f"warm-started from {SHIPPED_ONNX}, first conv widened to 5 input channels")

    opt = torch.optim.Adam(net.parameters(), lr=LR)

    for epoch in range(EPOCHS):
        total_hole = 0.0
        total_perc = 0.0
        total_div = 0.0
        for _ in range(STEPS_PER_EPOCH):
            gt = torch.stack([random_patch(random.choice(images)) for _ in range(BATCH)])
            mask = torch.stack([random_mask() for _ in range(BATCH)])
            masked_input = gt * (1 - mask)
            z1 = torch.randn(BATCH, 1, PATCH, PATCH)
            z2 = torch.randn(BATCH, 1, PATCH, PATCH)

            out1 = net(torch.cat([masked_input, mask, z1], dim=1))
            out2 = net(torch.cat([masked_input, mask, z2], dim=1))

            hole_area = mask.sum(dim=[1, 2, 3]).clamp(min=1.0)
            context_area = (1 - mask).sum(dim=[1, 2, 3]).clamp(min=1.0)

            def hole_l1(out):
                return (((out - gt).abs() * mask).sum(dim=[1, 2, 3]) / hole_area).mean()

            def context_l1(out):
                return (((out - gt).abs() * (1 - mask)).sum(dim=[1, 2, 3]) / context_area).mean()

            hole_loss = 0.5 * (hole_l1(out1) + hole_l1(out2))
            context_loss = 0.5 * (context_l1(out1) + context_l1(out2))

            composite1 = gt * (1 - mask) + out1 * mask
            perceptual_loss = nn.functional.mse_loss(
                vgg(composite1)["relu2_2"], vgg(gt)["relu2_2"]
            )
            tv_loss = total_variation(composite1)

            # Mode seeking: per sample, output distance inside the hole
            # over noise distance; the loss is its reciprocal, so
            # identical outputs for different noise are expensive.
            d_out = ((out1 - out2).abs() * mask).sum(dim=[1, 2, 3]) / hole_area / 3.0
            d_z = (z1 - z2).abs().mean(dim=[1, 2, 3])
            mode_seeking_loss = (1.0 / (d_out / d_z + MODE_SEEKING_EPS)).mean()

            loss = (
                HOLE_WEIGHT * hole_loss
                + CONTEXT_WEIGHT * context_loss
                + PERCEPTUAL_WEIGHT * perceptual_loss
                + MODE_SEEKING_WEIGHT * mode_seeking_loss
                + TV_WEIGHT * tv_loss
            )
            opt.zero_grad()
            loss.backward()
            torch.nn.utils.clip_grad_norm_(net.parameters(), max_norm=5.0)
            opt.step()

            if not torch.isfinite(loss):
                raise RuntimeError(
                    f"Training diverged (non-finite loss) at epoch {epoch+1}; aborting "
                    "rather than saving a broken checkpoint."
                )

            total_hole += hole_loss.item()
            total_perc += perceptual_loss.item()
            total_div += d_out.mean().item() * 255.0

        print(
            f"epoch {epoch+1}/{EPOCHS}  "
            f"hole_l1 {total_hole/STEPS_PER_EPOCH:.4f}  "
            f"perceptual {total_perc/STEPS_PER_EPOCH:.4f}  "
            f"diversity {total_div/STEPS_PER_EPOCH:.2f}/255"
        )
        if (epoch + 1) % CHECKPOINT_EVERY == 0 or epoch + 1 == EPOCHS:
            save_checkpoint_atomic(net, "inpaint_net_seeded.pt")
            print(f"checkpointed inpaint_net_seeded.pt at epoch {epoch+1}")

    print("done")


if __name__ == "__main__":
    main()
