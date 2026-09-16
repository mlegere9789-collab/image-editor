"""
A small class- and caption-conditional denoising diffusion model
(Ho, Jain & Abbeel, "Denoising Diffusion Probabilistic Models", NeurIPS
2020; cosine noise schedule from Nichol & Dhariwal, "Improved Denoising
Diffusion Probabilistic Models", ICML 2021; classifier-free guidance
from Ho & Salimans, "Classifier-Free Diffusion Guidance", 2022) --
standard, published architecture and objective, reimplemented here (not
copied from anywhere) at a size a 4-thread CPU can train: a UNet at
64x64 with channel widths 64/128/192, two residual blocks per level,
self-attention only at 16x16, ~7M parameters.

Conditioning is real and comes from this project's own training data:
each of the 787 landscape photographs carries one of 7 real categories
and a real Flickr title. The category goes through an embedding (index
7 is the "no category" null); the title goes through a bag-of-words
over a small vocabulary built from those titles (`build_vocab`) and a
linear projection (an all-zero bag is the "no caption" null). Both are
added to the timestep embedding, so every residual block sees them.
Dropping the condition 10% of the time during training is what makes
classifier-free guidance possible at sampling time.

Every parameter starts random; no pretrained checkpoint of any kind is
loaded anywhere.
"""

import json
import math
import re

import torch
from torch import nn

IMAGE_SIZE = 64
CATEGORIES = ["city", "field", "forest", "lake", "mountain", "ocean", "road"]
NULL_CATEGORY = len(CATEGORIES)
VOCAB_SIZE = 256
STOPWORDS = {
    "the", "and", "with", "from", "over", "into", "near", "for", "this", "that",
    "are", "was", "you", "your", "its", "our", "off", "out", "one", "two",
    "three", "img", "dsc", "jpg", "photo", "picture", "image", "view", "shot",
    "day", "trip", "some", "all", "not", "but", "has", "have", "had", "his",
    "her", "they", "them", "their", "there", "here", "where", "when", "what",
    "than", "then", "also", "just", "very", "more", "most", "such", "only",
}


def tokenize(text):
    return [t for t in re.findall(r"[a-z]+", text.lower()) if len(t) >= 3 and t not in STOPWORDS]


def build_vocab(attributions):
    """The VOCAB_SIZE most frequent title tokens, the 7 category words
    always included, in a fixed order -- saved to vocab.json so the Rust
    side tokenises a prompt identically."""
    counts = {}
    for a in attributions:
        for t in tokenize(a.get("title", "")):
            counts[t] = counts.get(t, 0) + 1
    words = list(CATEGORIES)
    for w, _ in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])):
        if w not in words:
            words.append(w)
        if len(words) == VOCAB_SIZE:
            break
    return words


def caption_vector(text, vocab):
    index = {w: i for i, w in enumerate(vocab)}
    v = torch.zeros(len(vocab))
    for t in tokenize(text):
        if t in index:
            v[index[t]] = 1.0
    return v


def category_of_prompt(text):
    """The first category word a prompt mentions, else the null category."""
    for t in tokenize(text):
        if t in CATEGORIES:
            return CATEGORIES.index(t)
    return NULL_CATEGORY


def cosine_alphas_cumprod(timesteps, s=0.008):
    steps = torch.arange(timesteps + 1, dtype=torch.float64)
    f = torch.cos(((steps / timesteps) + s) / (1 + s) * math.pi / 2) ** 2
    alphas_cumprod = f / f[0]
    betas = 1 - alphas_cumprod[1:] / alphas_cumprod[:-1]
    betas = betas.clamp(max=0.999)
    return torch.cumprod(1 - betas, dim=0).float()


def timestep_embedding(t, dim):
    half = dim // 2
    freqs = torch.exp(-math.log(10000) * torch.arange(half, dtype=torch.float32) / half)
    args = t.float()[:, None] * freqs[None]
    return torch.cat([torch.cos(args), torch.sin(args)], dim=1)


class ResBlock(nn.Module):
    def __init__(self, in_ch, out_ch, emb_dim):
        super().__init__()
        self.norm1 = nn.GroupNorm(8, in_ch)
        self.conv1 = nn.Conv2d(in_ch, out_ch, 3, padding=1)
        self.emb = nn.Linear(emb_dim, out_ch)
        self.norm2 = nn.GroupNorm(8, out_ch)
        self.conv2 = nn.Conv2d(out_ch, out_ch, 3, padding=1)
        self.skip = nn.Conv2d(in_ch, out_ch, 1) if in_ch != out_ch else nn.Identity()

    def forward(self, x, emb):
        h = self.conv1(nn.functional.silu(self.norm1(x)))
        h = h + self.emb(nn.functional.silu(emb))[:, :, None, None]
        h = self.conv2(nn.functional.silu(self.norm2(h)))
        return h + self.skip(x)


class Attention(nn.Module):
    def __init__(self, ch):
        super().__init__()
        self.norm = nn.GroupNorm(8, ch)
        self.qkv = nn.Conv2d(ch, ch * 3, 1)
        self.out = nn.Conv2d(ch, ch, 1)

    def forward(self, x):
        b, c, h, w = x.shape
        q, k, v = self.qkv(self.norm(x)).reshape(b, 3, c, h * w).unbind(1)
        attn = torch.softmax(torch.bmm(q.transpose(1, 2), k) / math.sqrt(c), dim=-1)
        out = torch.bmm(v, attn.transpose(1, 2)).reshape(b, c, h, w)
        return x + self.out(out)


class UNet(nn.Module):
    def __init__(self, vocab_size=VOCAB_SIZE, base=64, mults=(1, 2, 3), emb_dim=256):
        super().__init__()
        self.time_mlp = nn.Sequential(nn.Linear(128, emb_dim), nn.SiLU(), nn.Linear(emb_dim, emb_dim))
        self.category = nn.Embedding(NULL_CATEGORY + 1, emb_dim)
        self.caption = nn.Linear(vocab_size, emb_dim)
        chans = [base * m for m in mults]
        self.inp = nn.Conv2d(3, base, 3, padding=1)
        self.down = nn.ModuleList()
        self.downsample = nn.ModuleList()
        ch = base
        skip_chans = [base]
        for level, out_ch in enumerate(chans):
            blocks = nn.ModuleList()
            for _ in range(2):
                blocks.append(ResBlock(ch, out_ch, emb_dim))
                ch = out_ch
                skip_chans.append(ch)
            self.down.append(blocks)
            if level < len(chans) - 1:
                self.downsample.append(nn.Conv2d(ch, ch, 3, stride=2, padding=1))
                skip_chans.append(ch)
        self.mid1 = ResBlock(ch, ch, emb_dim)
        self.mid_attn = Attention(ch)
        self.mid2 = ResBlock(ch, ch, emb_dim)
        self.up = nn.ModuleList()
        self.upsample = nn.ModuleList()
        for level, out_ch in reversed(list(enumerate(chans))):
            blocks = nn.ModuleList()
            for _ in range(3):
                blocks.append(ResBlock(ch + skip_chans.pop(), out_ch, emb_dim))
                ch = out_ch
            self.up.append(blocks)
            if level > 0:
                self.upsample.append(nn.Conv2d(ch, ch, 3, padding=1))
        self.attn_level = len(chans) - 1
        self.out_norm = nn.GroupNorm(8, ch)
        self.out = nn.Conv2d(ch, 3, 3, padding=1)

    def forward(self, x, t, category, caption):
        return self.forward_with_embedding(x, timestep_embedding(t, 128), category, caption)

    def forward_with_embedding(self, x, temb, category, caption):
        """The forward pass from a precomputed sinusoidal timestep
        embedding -- what the exported graph takes, so the cos/sin of
        large arguments is computed by the caller in full precision
        rather than by whichever runtime evaluates the graph."""
        emb = self.time_mlp(temb) + self.category(category) + self.caption(caption)
        h = self.inp(x)
        skips = [h]
        for level, blocks in enumerate(self.down):
            for block in blocks:
                h = block(h, emb)
                skips.append(h)
            if level < len(self.downsample):
                h = self.downsample[level](h)
                skips.append(h)
        h = self.mid2(self.mid_attn(self.mid1(h, emb)), emb)
        for i, blocks in enumerate(self.up):
            for block in blocks:
                h = block(torch.cat([h, skips.pop()], dim=1), emb)
            if i < len(self.upsample):
                # Nearest x2 written as a repeat: the same numbers as
                # F.interpolate(mode="nearest"), exported as plain reshapes
                # that every ONNX runtime resolves identically (a Resize op's
                # coordinate rounding is where runtimes quietly differ).
                h = h.repeat_interleave(2, dim=2).repeat_interleave(2, dim=3)
                h = self.upsample[i](h)
        return self.out(nn.functional.silu(self.out_norm(h)))


def load_vocab(path="vocab.json"):
    with open(path) as f:
        return json.load(f)
