"""Hole-region L1 vs ground truth (0-255 units) on four held-out holes:
the shipped 4-channel model vs the seeded checkpoint, the latter as the
MEAN over four seeds (what a random variation costs) and the BEST of
those four (what picking between variations gets). The measurement the
mode-seeking weight was tuned against."""
import sys, torch, numpy as np
from PIL import Image
from inpaint_net import InpaintNet
from train_similar import shipped_state_dict, SHIPPED_ONNX

SEEDS = (1, 2, 3, 4)
base = InpaintNet(in_channels=4); base.load_state_dict(shipped_state_dict(SHIPPED_ONNX)); base.eval()
seeded = InpaintNet(in_channels=5)
seeded.load_state_dict(torch.load(sys.argv[1] if len(sys.argv) > 1 else "inpaint_net_seeded.pt", map_location="cpu", weights_only=True)); seeded.eval()

cases = [("raw/ocean__3736154699_69369c72c3.jpg", 50, 78, 50, 78),
         ("raw/city__4469359354_78f23ef964.jpg", 50, 78, 50, 78),
         ("raw/road__2994421437_e9f337b4d5.jpg", 50, 78, 50, 78),
         ("raw/mountain__24508553818_c782c29843.jpg", 40, 88, 40, 88)]
tb = tm = tbest = 0.0
for path, y0, y1, x0, x1 in cases:
    im = Image.open(path).convert("RGB").resize((128, 128))
    gt = torch.from_numpy(np.array(im, dtype="float32") / 255.0).permute(2, 0, 1).unsqueeze(0)
    mask = torch.zeros(1, 1, 128, 128); mask[:, :, y0:y1, x0:x1] = 1.0
    masked = gt * (1 - mask)
    area = mask.sum() * 3
    with torch.no_grad():
        lb = (((base(torch.cat([masked, mask], dim=1)) - gt).abs() * mask).sum() / area * 255).item()
        ls = []
        for s in SEEDS:
            g = torch.Generator().manual_seed(s)
            o = seeded(torch.cat([masked, mask, torch.randn(1, 1, 128, 128, generator=g)], dim=1))
            ls.append((((o - gt).abs() * mask).sum() / area * 255).item())
    mean_s, best_s = sum(ls) / len(ls), min(ls)
    tb += lb; tm += mean_s; tbest += best_s
    print(f"{path.split('/')[-1][:12]:12s} shipped {lb:6.2f}  seeded mean-of-4 {mean_s:6.2f}  best-of-4 {best_s:6.2f}")
n = len(cases)
print(f"{'MEAN':12s} shipped {tb/n:6.2f}  seeded mean-of-4 {tm/n:6.2f}  best-of-4 {tbest/n:6.2f}")
