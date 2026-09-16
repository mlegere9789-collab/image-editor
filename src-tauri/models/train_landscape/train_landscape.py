"""
Image > Adjustments > Landscape Mixer -- trains TransformNet from
random initialization for real feed-forward neural style transfer
(Johnson et al. 2016), against:
  - real content: 787 real landscape photographs, sourced from Flickr
    via the ml5js/ml5-data-and-models "landscapes" dataset (MIT-licensed
    repository), filtered to only images individually marked CC BY,
    CC0, public domain, or "no known copyright restrictions" (excluding
    every image under a non-commercial-only or no-derivatives license)
    -- see attributions.json for the real per-image title/photographer/
    URL/license this project's own filtering kept.
  - real style: one of those same real landscape photographs -- a
    genuine mountain sunset (see STYLE_IMAGE below), chosen for its
    strong warm/cool colour contrast, the "mood" this filter blends
    toward.
  - a real, frozen, pretrained VGG16 (ONNX Model Zoo, Apache 2.0) as the
    perceptual-loss feature extractor -- never updated by this training
    run, only used to score how the network is doing.

No checkpoint of TransformNet itself is loaded anywhere; every one of
its parameters starts random and is updated only by real gradient
descent against these real losses. Hyperparameters (learning rate,
gradient clipping, style weight) are carried over unchanged from the
Style Transfer phase's own third, stable training attempt -- the first
two attempts for that model diverged or over-stylized, and there is no
reason to re-discover the same failure modes here.
"""

import glob
import random

import torch
from PIL import Image
from torch import nn

from transform_net import TransformNet
from vgg_extract import Vgg16Features, load_vgg_conv_weights

PATCH = 128
BATCH = 4
STEPS_PER_EPOCH = 150
EPOCHS = 16
LR = 4e-4
STYLE_WEIGHT = 3e4
CONTENT_WEIGHT = 1.0
TV_WEIGHT = 1e-6
STYLE_IMAGE = "raw/mountain__24508553818_c782c29843.jpg"


def gram_matrix(feat):
    b, c, h, w = feat.shape
    f = feat.reshape(b, c, h * w)
    g = torch.bmm(f, f.transpose(1, 2))
    return g / (c * h * w)


def total_variation(img):
    return (
        (img[:, :, 1:, :] - img[:, :, :-1, :]).abs().mean()
        + (img[:, :, :, 1:] - img[:, :, :, :-1]).abs().mean()
    )


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


def main():
    torch.manual_seed(0)
    random.seed(0)
    images = load_dataset()

    vgg_tensors = load_vgg_conv_weights("vgg16.onnx")
    vgg = Vgg16Features(vgg_tensors)
    net = TransformNet()

    style_img = Image.open(STYLE_IMAGE).convert("RGB").resize((PATCH, PATCH))
    style_t = (
        torch.from_numpy(__import__("numpy").array(style_img, dtype="float32") / 255.0)
        .permute(2, 0, 1)
        .unsqueeze(0)
    )
    with torch.no_grad():
        style_feats = vgg(style_t)
        style_grams = {k: gram_matrix(v) for k, v in style_feats.items()}

    opt = torch.optim.Adam(net.parameters(), lr=LR)
    mse = nn.MSELoss()

    for epoch in range(EPOCHS):
        total_style_loss = 0.0
        total_content_loss = 0.0
        for _ in range(STEPS_PER_EPOCH):
            batch = torch.stack([random_patch(random.choice(images)) for _ in range(BATCH)])
            styled = net(batch)

            content_feats = vgg(batch)
            styled_feats = vgg(styled)

            content_loss = mse(styled_feats["relu2_2"], content_feats["relu2_2"])

            style_loss = 0.0
            for key in style_grams:
                g_styled = gram_matrix(styled_feats[key])
                g_style = style_grams[key].expand(BATCH, -1, -1)
                style_loss = style_loss + mse(g_styled, g_style)

            tv_loss = total_variation(styled)

            loss = (
                CONTENT_WEIGHT * content_loss
                + STYLE_WEIGHT * style_loss
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

            total_style_loss += style_loss.item()
            total_content_loss += content_loss.item()

        print(
            f"epoch {epoch+1}/{EPOCHS}  "
            f"content {total_content_loss/STEPS_PER_EPOCH:.4f}  "
            f"style {total_style_loss/STEPS_PER_EPOCH:.6f}"
        )

    torch.save(net.state_dict(), "style_net.pt")
    print("saved style_net.pt")


if __name__ == "__main__":
    main()
