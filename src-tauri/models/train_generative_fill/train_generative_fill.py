"""
Trains InpaintNet from random initialization for real, self-supervised
generative fill (Pathak et al. 2016's own self-supervision recipe: mask
a real region of a real photo, train the network to hallucinate it back
from the surrounding context alone -- no labels needed, so any real,
appropriately-licensed photograph is real training data) against:
  - real content: the same 787 real, individually-licensed landscape
    photographs `../train_landscape` already fetched and attributed
    (`attributions.json` in this directory, copied from there) -- more
    scene diversity than the 51-photo OpenCV set this project's earlier
    models used, which matters more here than for style transfer since
    this network has to invent plausible *content*, not just re-tone
    pixels that are already there.
  - a real, frozen, pretrained VGG16 (ONNX Model Zoo, Apache 2.0) as the
    perceptual-loss feature extractor on the *composited* result (known
    pixels + hallucinated hole), never updated by this training run.

No checkpoint of InpaintNet itself is loaded anywhere; every one of its
parameters starts random and is updated only by real gradient descent.

This is deliberately NOT a GAN: an adversarial loss is the standard way
context encoders sharpen their output, but this project's own style
transfer training already needed three attempts to find a stable
non-adversarial recipe (see ../STYLE_TRANSFER_NOTICE.md) -- adding a
second, jointly-trained discriminator network is a well-known source of
training instability (mode collapse, oscillation) on top of that, and
this recipe already has a working stable alternative: perceptual loss
against a frozen network, the same technique the style-transfer/
restoration/landscape-mixer models already proved converges reliably in
this sandbox. The honest tradeoff: reconstructions are real and
context-aware but softer/blurrier than a real GAN or diffusion model's
would be -- see GENERATIVE_FILL_NOTICE.md for the full, honest writeup.

Loss weights here reflect a real lesson from this project's own first
attempt at this model (a plain bottleneck, no skip connections): a
perceptual weight as high as the style-transfer recipe's own let the
network satisfy the loss with "plausible-looking texture in the
abstract" rather than the actual local colour that belongs in the hole,
particularly in low-texture regions. `HOLE_WEIGHT` dominates here for
that reason; perceptual loss stays in at a much smaller weight only to
discourage flat/dead output, not to compete with colour accuracy.
Checkpoints save every `CHECKPOINT_EVERY` epochs (atomically) so
training can be qualitatively checked without stopping the run.
"""

import glob
import os
import random

import torch
from PIL import Image
from torch import nn

from inpaint_net import InpaintNet
from vgg_extract import Vgg16Features, load_vgg_conv_weights

PATCH = 128
BATCH = 4
STEPS_PER_EPOCH = 200
EPOCHS = 40
LR = 3e-4
HOLE_WEIGHT = 3.0
CONTEXT_WEIGHT = 0.3
PERCEPTUAL_WEIGHT = 0.08
TV_WEIGHT = 1e-6
HOLE_MIN = 32
HOLE_MAX = 64
CHECKPOINT_EVERY = 5


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
    arr = torch.from_numpy(
        __import__("numpy").array(patch, dtype="float32") / 255.0
    ).permute(2, 0, 1)
    return arr


def random_mask():
    """A single random rectangular hole within the PATCH, [0, 1] over
    a (1, PATCH, PATCH) tensor -- 1 inside the hole, 0 outside."""
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


def save_checkpoint_atomic(net, path):
    tmp = path + ".tmp"
    torch.save(net.state_dict(), tmp)
    os.replace(tmp, path)


def main():
    torch.manual_seed(0)
    random.seed(0)
    images = load_dataset()

    vgg_tensors = load_vgg_conv_weights("vgg16.onnx")
    vgg = Vgg16Features(vgg_tensors)
    net = InpaintNet()

    opt = torch.optim.Adam(net.parameters(), lr=LR)

    for epoch in range(EPOCHS):
        total_hole = 0.0
        total_perc = 0.0
        for _ in range(STEPS_PER_EPOCH):
            gt = torch.stack([random_patch(random.choice(images)) for _ in range(BATCH)])
            mask = torch.stack([random_mask() for _ in range(BATCH)])

            masked_input = gt * (1 - mask)
            net_input = torch.cat([masked_input, mask], dim=1)
            predicted = net(net_input)
            composite = gt * (1 - mask) + predicted * mask

            hole_area = mask.sum(dim=[1, 2, 3]).clamp(min=1.0)
            hole_l1 = ((predicted - gt).abs() * mask).sum(dim=[1, 2, 3]) / hole_area
            hole_loss = hole_l1.mean()

            context_area = (1 - mask).sum(dim=[1, 2, 3]).clamp(min=1.0)
            context_l1 = ((predicted - gt).abs() * (1 - mask)).sum(dim=[1, 2, 3]) / context_area
            context_loss = context_l1.mean()

            composite_feats = vgg(composite)
            gt_feats = vgg(gt)
            perceptual_loss = nn.functional.mse_loss(
                composite_feats["relu2_2"], gt_feats["relu2_2"]
            )

            tv_loss = total_variation(composite)

            loss = (
                HOLE_WEIGHT * hole_loss
                + CONTEXT_WEIGHT * context_loss
                + PERCEPTUAL_WEIGHT * perceptual_loss
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

        print(
            f"epoch {epoch+1}/{EPOCHS}  "
            f"hole_l1 {total_hole/STEPS_PER_EPOCH:.4f}  "
            f"perceptual {total_perc/STEPS_PER_EPOCH:.4f}"
        )
        if (epoch + 1) % CHECKPOINT_EVERY == 0 or epoch + 1 == EPOCHS:
            save_checkpoint_atomic(net, "inpaint_net.pt")
            print(f"checkpointed inpaint_net.pt at epoch {epoch+1}")

    print("done")


if __name__ == "__main__":
    main()
