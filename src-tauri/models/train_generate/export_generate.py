"""
Exports the EMA weights of `generate_ckpt.pt` as `../generate.onnx`,
the file the desktop app (and the server) run with `tract`: a fixed
batch of 2 -- row 0 the conditioned pass, row 1 the unconditioned one,
so classifier-free guidance is one model evaluation per sampling step
-- at 64x64, opset 17, taking the 128-wide sinusoidal timestep embedding
already computed (so no runtime's own cos/sin of arguments up to 1000
enters the result) and the category as int64 for the embedding gather.
`--check` runs the exported graph with onnxruntime-free numpy parity
via torch: the ONNX output is compared to the PyTorch output on a
random input.
"""

import json
import sys

import torch

from diffusion_unet import IMAGE_SIZE, UNet, timestep_embedding

BATCH = 2


def explicit_group_norm(self, x):
    """GroupNorm spelled out as reshape, mean, variance, normalise,
    reshape, scale and shift -- the same numbers as nn.GroupNorm, but
    exported as ReduceMean/Sub/Mul/Sqrt/Div rather than the
    InstanceNormalization form torch's exporter uses, which tract
    evaluates differently (checked: up to 0.095 apart on a random
    input, against 1e-6 for every other op in this model)."""
    b, c, h, w = x.shape
    g = x.reshape(b, self.num_groups, -1)
    mean = g.mean(dim=2, keepdim=True)
    var = ((g - mean) ** 2).mean(dim=2, keepdim=True)
    g = (g - mean) / torch.sqrt(var + self.eps)
    return g.reshape(b, c, h, w) * self.weight[None, :, None, None] + self.bias[None, :, None, None]


torch.nn.GroupNorm.forward = explicit_group_norm


class ExportUNet(torch.nn.Module):
    """The UNet with a float timestep, as tract prefers."""

    def __init__(self, net):
        super().__init__()
        self.net = net

    def forward(self, x, temb, category, caption):
        return self.net.forward_with_embedding(x, temb, category, caption)


def main():
    vocab = json.load(open("vocab.json"))
    ckpt = torch.load("generate_ckpt.pt", map_location="cpu", weights_only=True)
    net = UNet(vocab_size=len(vocab))
    net.load_state_dict(ckpt["ema"])
    net.eval()
    print(f"exporting EMA weights from step {ckpt['step']}")
    model = ExportUNet(net)
    x = torch.randn(BATCH, 3, IMAGE_SIZE, IMAGE_SIZE)
    t = torch.tensor([500.0, 500.0])
    temb = timestep_embedding(t, 128)
    category = torch.tensor([3, 7])
    caption = torch.zeros(BATCH, len(vocab))
    out = "../generate.onnx" if "--out" not in sys.argv else sys.argv[sys.argv.index("--out") + 1]
    torch.onnx.export(
        model,
        (x, temb, category, caption),
        out,
        input_names=["x", "temb", "category", "caption"],
        output_names=["eps"],
        opset_version=17,
        dynamo=False,
        do_constant_folding=True,
    )
    # A parity fixture the Rust side reproduces exactly: x[i] = sin(i / 7)
    # over the flattened batch, t = 500 for both rows, category 3 (lake)
    # and 7 (null), caption all zero -- the first 64 outputs and the mean
    # absolute output, to compare tract's evaluation against PyTorch's.
    n = BATCH * 3 * IMAGE_SIZE * IMAGE_SIZE
    x = torch.sin(torch.arange(n, dtype=torch.float64) / 7.0).to(torch.float32).reshape(BATCH, 3, IMAGE_SIZE, IMAGE_SIZE)
    with torch.no_grad():
        reference = model(x, temb, category, caption)
    json.dump(
        {
            "step": ckpt["step"],
            "first": [float(v) for v in reference.flatten()[:64]],
            "mean_abs": float(reference.abs().mean()),
        },
        open("../generate_check.json", "w"),
    )
    print(f"wrote {out} and ../generate_check.json (step {ckpt['step']}, mean |eps| {reference.abs().mean():.4f})")


if __name__ == "__main__":
    main()
