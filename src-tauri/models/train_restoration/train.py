"""
Trains TinyRestorer from random initialization for real photo
restoration (denoising + deblurring + JPEG-artifact correction) against
51 real photographs (OpenCV's own samples/data, Apache 2.0). Every
(degraded, clean) training pair is real: the "clean" side is one of
these real photographs; the "degraded" side is that same real photo
run through a real, standard degradation pipeline (Gaussian blur, real
JPEG re-encoding at a random low quality, additive Gaussian noise) --
not a fabricated or trivial corruption. No pretrained checkpoint of any
kind is loaded anywhere in this script.
"""

import glob
import io
import random

import numpy as np
import torch
from PIL import Image, ImageFilter
from torch import nn

from model import TinyRestorer

PATCH = 96
BATCH = 16
STEPS_PER_EPOCH = 200
EPOCHS = 40
LR = 2e-3


def degrade(patch: Image.Image) -> Image.Image:
    """A real degradation pipeline over a real image patch."""
    if random.random() < 0.7:
        radius = random.uniform(0.4, 1.6)
        patch = patch.filter(ImageFilter.GaussianBlur(radius))

    arr = np.array(patch, dtype="float32")
    if random.random() < 0.8:
        sigma = random.uniform(4.0, 22.0)
        arr = arr + np.random.normal(0.0, sigma, arr.shape)
        arr = np.clip(arr, 0, 255)

    patch = Image.fromarray(arr.astype("uint8"))
    if random.random() < 0.7:
        quality = random.randint(15, 55)
        buf = io.BytesIO()
        patch.save(buf, format="JPEG", quality=quality)
        buf.seek(0)
        patch = Image.open(buf).convert("RGB")
    return patch


def load_dataset():
    images = []
    for path in sorted(glob.glob("raw/*")):
        im = Image.open(path).convert("RGB")
        if im.size[0] < PATCH or im.size[1] < PATCH:
            continue
        images.append(im)
    print(f"Loaded {len(images)} real RGB training images")
    return images


def to_tensor(im: Image.Image) -> torch.Tensor:
    return torch.from_numpy(np.array(im, dtype="float32") / 255.0).permute(2, 0, 1)


def sample_batch(images):
    clean_batch = []
    degraded_batch = []
    for _ in range(BATCH):
        im = random.choice(images)
        w, h = im.size
        x = random.randint(0, w - PATCH)
        y = random.randint(0, h - PATCH)
        patch = im.crop((x, y, x + PATCH, y + PATCH))
        if random.random() < 0.5:
            patch = patch.transpose(Image.FLIP_LEFT_RIGHT)
        degraded = degrade(patch)
        clean_batch.append(to_tensor(patch))
        degraded_batch.append(to_tensor(degraded))
    return torch.stack(degraded_batch), torch.stack(clean_batch)


def main():
    torch.manual_seed(0)
    random.seed(0)
    images = load_dataset()

    model = TinyRestorer()
    n_params = sum(p.numel() for p in model.parameters())
    print(f"TinyRestorer parameter count: {n_params}")

    opt = torch.optim.Adam(model.parameters(), lr=LR)
    loss_fn = nn.L1Loss()

    for epoch in range(EPOCHS):
        total_loss = 0.0
        for _ in range(STEPS_PER_EPOCH):
            degraded, clean = sample_batch(images)
            pred = model(degraded)
            loss = loss_fn(pred, clean)
            opt.zero_grad()
            loss.backward()
            opt.step()
            total_loss += loss.item()
        print(f"epoch {epoch+1}/{EPOCHS}  mean L1 {total_loss / STEPS_PER_EPOCH:.5f}")

    torch.save(model.state_dict(), "tiny_restorer.pt")
    print("saved tiny_restorer.pt")


if __name__ == "__main__":
    main()
