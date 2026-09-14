"""
Trains the conditional DDPM in `diffusion_unet.py` from random
initialisation on the 787 real, individually-licensed landscape
photographs (`attributions.json`, the same set and attribution as
`../train_landscape`), conditioned on each photo's real category and
real Flickr title. Epsilon-prediction MSE (Ho et al. 2020), cosine
schedule, T = 1000, EMA weights, classifier-free-guidance dropout of
the condition 10% of the time.

Long-running by design: atomic checkpoints every CHECKPOINT_EVERY
steps to `generate_ckpt.pt` (model, EMA, optimizer, step), `--resume`
to continue, and a 2x4 sample grid from the EMA weights every
SAMPLE_EVERY steps to `samples/` (one column per category, DDIM 25
steps, guidance 2.0) so training can be judged by eye as it goes, not
only by the loss. The log prints seconds per step so the time to
TARGET_STEPS is a measurement, not a guess.
"""

import glob
import json
import os
import random
import sys
import time

import numpy as np
import torch
from PIL import Image

from diffusion_unet import (
    CATEGORIES,
    IMAGE_SIZE,
    NULL_CATEGORY,
    UNet,
    build_vocab,
    caption_vector,
    cosine_alphas_cumprod,
)

BATCH = 8
LR = 2e-4
TARGET_STEPS = 60_000
CHECKPOINT_EVERY = 500
SAMPLE_EVERY = 2_500
LOG_EVERY = 50
TIMESTEPS = 1000
COND_DROPOUT = 0.1
EMA_DECAY = 0.9995
RESIZE_SHORT = 72


def load_dataset(vocab):
    attrs = {a["file"]: a for a in json.load(open("attributions.json"))}
    items = []
    for path in sorted(glob.glob("raw/*")):
        a = attrs.get(os.path.basename(path))
        if a is None:
            continue
        im = Image.open(path).convert("RGB")
        w, h = im.size
        scale = RESIZE_SHORT / min(w, h)
        im = im.resize((max(IMAGE_SIZE, round(w * scale)), max(IMAGE_SIZE, round(h * scale))), Image.BICUBIC)
        items.append((im, CATEGORIES.index(a["category"]), caption_vector(a.get("title", ""), vocab)))
    print(f"Loaded {len(items)} real captioned, categorised training images")
    return items


def random_crop(im):
    w, h = im.size
    x = random.randint(0, w - IMAGE_SIZE)
    y = random.randint(0, h - IMAGE_SIZE)
    patch = im.crop((x, y, x + IMAGE_SIZE, y + IMAGE_SIZE))
    if random.random() < 0.5:
        patch = patch.transpose(Image.FLIP_LEFT_RIGHT)
    return torch.from_numpy(np.array(patch, dtype="float32") / 127.5 - 1.0).permute(2, 0, 1)


def save_atomic(state, path):
    tmp = path + ".tmp"
    torch.save(state, tmp)
    os.replace(tmp, path)


@torch.no_grad()
def ddim_sample(net, alphas_cumprod, category, caption, steps=25, guidance=2.0, seed=0):
    g = torch.Generator().manual_seed(seed)
    n = category.shape[0]
    x = torch.randn(n, 3, IMAGE_SIZE, IMAGE_SIZE, generator=g)
    ts = torch.linspace(TIMESTEPS - 1, 0, steps).round().long()
    null_cat = torch.full_like(category, NULL_CATEGORY)
    null_cap = torch.zeros_like(caption)
    for i, t in enumerate(ts):
        tb = torch.full((n,), int(t), dtype=torch.long)
        eps_c = net(x, tb, category, caption)
        eps_u = net(x, tb, null_cat, null_cap)
        eps = eps_u + guidance * (eps_c - eps_u)
        a_t = alphas_cumprod[t]
        x0 = ((x - (1 - a_t).sqrt() * eps) / a_t.sqrt()).clamp(-1, 1)
        if i + 1 < len(ts):
            a_prev = alphas_cumprod[ts[i + 1]]
            x = a_prev.sqrt() * x0 + (1 - a_prev).sqrt() * eps
        else:
            x = x0
    return x


def sample_grid(ema, alphas_cumprod, vocab, step):
    ema.eval()
    prompts = CATEGORIES + ["sunset"]
    category = torch.tensor([CATEGORIES.index(p) if p in CATEGORIES else NULL_CATEGORY for p in prompts])
    caption = torch.stack([caption_vector(p, vocab) for p in prompts])
    imgs = ddim_sample(ema, alphas_cumprod, category, caption)
    rows = [torch.cat(list(imgs[:4]), dim=2), torch.cat(list(imgs[4:]), dim=2)]
    grid = torch.cat(rows, dim=1)
    arr = ((grid.permute(1, 2, 0).numpy() + 1) * 127.5).clip(0, 255).astype("uint8")
    os.makedirs("samples", exist_ok=True)
    Image.fromarray(arr).resize((arr.shape[1] * 2, arr.shape[0] * 2), Image.NEAREST).save(f"samples/step_{step:06d}.png")
    ema.train()


def main():
    torch.manual_seed(0)
    random.seed(0)
    if os.path.exists("vocab.json"):
        vocab = json.load(open("vocab.json"))
    else:
        vocab = build_vocab(json.load(open("attributions.json")))
        json.dump(vocab, open("vocab.json", "w"), indent=0)
    print(f"vocabulary: {len(vocab)} title tokens")
    items = load_dataset(vocab)
    alphas_cumprod = cosine_alphas_cumprod(TIMESTEPS)

    net = UNet(vocab_size=len(vocab))
    ema = UNet(vocab_size=len(vocab))
    ema.load_state_dict(net.state_dict())
    for p in ema.parameters():
        p.requires_grad_(False)
    opt = torch.optim.Adam(net.parameters(), lr=LR)
    step = 0
    if "--resume" in sys.argv and os.path.exists("generate_ckpt.pt"):
        ckpt = torch.load("generate_ckpt.pt", map_location="cpu", weights_only=True)
        net.load_state_dict(ckpt["model"])
        ema.load_state_dict(ckpt["ema"])
        opt.load_state_dict(ckpt["opt"])
        step = ckpt["step"]
        print(f"resumed at step {step}")
    print(f"parameters: {sum(p.numel() for p in net.parameters())/1e6:.2f}M")

    window_loss, window_time = 0.0, time.time()
    while step < TARGET_STEPS:
        batch = random.sample(items, BATCH)
        x0 = torch.stack([random_crop(im) for im, _, _ in batch])
        category = torch.tensor([c for _, c, _ in batch])
        caption = torch.stack([v for _, _, v in batch])
        drop = torch.rand(BATCH) < COND_DROPOUT
        category = torch.where(drop, torch.full_like(category, NULL_CATEGORY), category)
        caption = caption * (~drop).float()[:, None]

        t = torch.randint(0, TIMESTEPS, (BATCH,))
        noise = torch.randn_like(x0)
        a = alphas_cumprod[t][:, None, None, None]
        xt = a.sqrt() * x0 + (1 - a).sqrt() * noise
        loss = torch.nn.functional.mse_loss(net(xt, t, category, caption), noise)
        opt.zero_grad()
        loss.backward()
        torch.nn.utils.clip_grad_norm_(net.parameters(), 1.0)
        opt.step()
        if not torch.isfinite(loss):
            raise RuntimeError(f"Training diverged (non-finite loss) at step {step}; aborting.")
        step += 1

        decay = min(EMA_DECAY, (1 + step) / (10 + step))
        with torch.no_grad():
            for pe, pn in zip(ema.parameters(), net.parameters()):
                pe.mul_(decay).add_(pn, alpha=1 - decay)

        window_loss += loss.item()
        if step % LOG_EVERY == 0:
            now = time.time()
            print(f"step {step}/{TARGET_STEPS}  loss {window_loss/LOG_EVERY:.4f}  {(now-window_time)/LOG_EVERY:.2f} s/step", flush=True)
            window_loss, window_time = 0.0, now
        if step % CHECKPOINT_EVERY == 0:
            save_atomic({"model": net.state_dict(), "ema": ema.state_dict(), "opt": opt.state_dict(), "step": step}, "generate_ckpt.pt")
            print(f"checkpointed generate_ckpt.pt at step {step}", flush=True)
        if step % SAMPLE_EVERY == 0:
            sample_grid(ema, alphas_cumprod, vocab, step)
            print(f"sampled samples/step_{step:06d}.png", flush=True)
    print("done")


if __name__ == "__main__":
    main()
