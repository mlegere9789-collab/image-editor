"""
Trains TinyColorizer from random initialization on real photographs
(OpenCV's own permissively-licensed `samples/data`, Apache 2.0, fetched
directly from github.com/opencv/opencv). No pretrained weights are loaded
anywhere in this script -- every parameter starts random and is updated
only by gradient descent against these real images, run in this sandbox.

Colour space: BT.601 YCbCr, matching this project's own existing Rust
convention (`super_resolution::rgb_to_ycbcr`) -- Y is the model's input,
Cb/Cr are its target/output.
"""

import glob
import random

import torch
from PIL import Image
from torch import nn

from model import TinyColorizer

PATCH = 96
BATCH = 16
STEPS_PER_EPOCH = 200
EPOCHS = 40
LR = 2e-3


def rgb_to_y_cbcr(img_rgb: torch.Tensor) -> torch.Tensor:
    # img_rgb: 3xHxW, 0..1. Returns 3xHxW: Y, Cb, Cr (BT.601, matching
    # src-tauri/src/super_resolution.rs::rgb_to_ycbcr exactly).
    r, g, b = img_rgb[0], img_rgb[1], img_rgb[2]
    luma = 0.299 * r + 0.587 * g + 0.114 * b
    cb = 0.5 + (b - luma) / 1.772
    cr = 0.5 + (r - luma) / 1.402
    return torch.stack([luma, cb, cr], dim=0)


def load_dataset():
    images = []
    for path in sorted(glob.glob("raw/*")):
        im = Image.open(path).convert("RGB")
        if im.size[0] < PATCH or im.size[1] < PATCH:
            continue
        images.append(im)
    print(f"Loaded {len(images)} real RGB training images")
    return images


def sample_batch(images, device):
    patches = []
    for _ in range(BATCH):
        im = random.choice(images)
        w, h = im.size
        x = random.randint(0, w - PATCH)
        y = random.randint(0, h - PATCH)
        patch = im.crop((x, y, x + PATCH, y + PATCH))
        if random.random() < 0.5:
            patch = patch.transpose(Image.FLIP_LEFT_RIGHT)
        if random.random() < 0.5:
            patch = patch.transpose(Image.FLIP_TOP_BOTTOM)
        arr = torch.from_numpy(
            __import__("numpy").array(patch, dtype="float32") / 255.0
        ).permute(2, 0, 1)
        patches.append(arr)
    batch = torch.stack(patches, dim=0).to(device)
    ycbcr = torch.stack([rgb_to_y_cbcr(p) for p in batch], dim=0)
    y = ycbcr[:, 0:1, :, :]
    cbcr = ycbcr[:, 1:3, :, :] - 0.5
    return y, cbcr


def main():
    torch.manual_seed(0)
    random.seed(0)
    device = "cpu"
    images = load_dataset()

    model = TinyColorizer().to(device)
    n_params = sum(p.numel() for p in model.parameters())
    print(f"TinyColorizer parameter count: {n_params}")

    opt = torch.optim.Adam(model.parameters(), lr=LR)
    loss_fn = nn.MSELoss()

    for epoch in range(EPOCHS):
        total_loss = 0.0
        for _ in range(STEPS_PER_EPOCH):
            y, target_cbcr = sample_batch(images, device)
            pred = model(y)
            loss = loss_fn(pred, target_cbcr)
            opt.zero_grad()
            loss.backward()
            opt.step()
            total_loss += loss.item()
        print(f"epoch {epoch+1}/{EPOCHS}  mean MSE {total_loss / STEPS_PER_EPOCH:.6f}")

    torch.save(model.state_dict(), "tiny_colorizer.pt")
    print("saved tiny_colorizer.pt")


if __name__ == "__main__":
    main()
