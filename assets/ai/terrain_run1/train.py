"""DDPM conditional terrain heightmap generation, single-file training.
Cond: 23 sub-classes + 9 parents + 5 relief + 4 texture + 3 water.
FiLM-conditioned UNet, epsilon prediction, EMA, CFG (train-time dropout).
"""
import argparse
import csv
import json
import math
import os
import random
import time

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

# ---------------- constants ----------------
SUB_CLASSES = sorted(["alpine", "caldera_rim", "canyon_gorge", "cliff_coast", "delta",
                      "dissected_hills", "dune_sea", "fjord", "flat_plain", "glacier_highland",
                      "hamada", "high_plateau", "lava_plateau", "mesa_badlands", "mid_mountain",
                      "rocky_wadi", "rolling_hills", "salt_playa", "sandy_coast", "shield_volcano",
                      "stratovolcano", "tundra_lowland", "undulating_plain"])
PARENT_CLASSES = sorted(["coastal", "desert", "glacial", "hill", "mountain", "plain", "plateau", "volcanic"])
RELIEF_CLASSES = ["flat", "low", "mid", "high", "extreme"]
TEXTURE_CLASSES = ["smooth", "undulating", "fine_ridged", "coarse_rugged"]
WATER_CLASSES = ["land", "water_edge", "water_lots"]

S2I = {s: i for i, s in enumerate(SUB_CLASSES)}
P2I = {s: i for i, s in enumerate(PARENT_CLASSES)}
R2I = {s: i for i, s in enumerate(RELIEF_CLASSES)}
T2I = {s: i for i, s in enumerate(TEXTURE_CLASSES)}
W2I = {s: i for i, s in enumerate(WATER_CLASSES)}


# ---------------- noise schedule ----------------
def betas_linear(T, lo=1e-4, hi=0.02):
    return torch.linspace(lo, hi, T)


def betas_cosine(T, s=0.008):
    steps = torch.arange(T + 1, dtype=torch.float32) / T
    alpha_bar = torch.cos((steps + s) / (1 + s) * math.pi / 2) ** 2
    betas = torch.clip(1 - alpha_bar[1:] / alpha_bar[:-1], max=0.999)
    return betas


# ---------------- model ----------------
class ResBlock(nn.Module):
    def __init__(self, cin, cout, cond_dim, dropout=0.1):
        super().__init__()
        self.n1 = nn.GroupNorm(8, cin)
        self.c1 = nn.Conv2d(cin, cout, 3, padding=1)
        self.n2 = nn.GroupNorm(8, cout)
        self.c2 = nn.Conv2d(cout, cout, 3, padding=1)
        self.drop = nn.Dropout(dropout)
        self.skip = nn.Conv2d(cin, cout, 1) if cin != cout else nn.Identity()
        self.film = nn.Linear(cond_dim, cout * 2)

    def forward(self, x, c):
        h = F.silu(self.n1(x))
        h = self.c1(h)
        h = F.silu(self.n2(h))
        h = self.drop(h)
        h = self.c2(h)
        s, b = self.film(c).unsqueeze(-1).unsqueeze(-1).chunk(2, dim=1)
        h = h * (1 + s) + b
        return h + self.skip(x)


class AttnBlock(nn.Module):
    def __init__(self, cin, heads=4):
        super().__init__()
        self.q = nn.Conv2d(cin, cin, 1)
        self.k = nn.Conv2d(cin, cin, 1)
        self.v = nn.Conv2d(cin, cin, 1)
        self.o = nn.Conv2d(cin, cin, 1)
        self.n = nn.GroupNorm(8, cin)
        self.heads = heads

    def forward(self, x):
        B, C, H, W = x.shape
        q = self.q(x).reshape(B, self.heads, C // self.heads, H * W)
        k = self.k(x).reshape(B, self.heads, C // self.heads, H * W)
        v = self.v(x).reshape(B, self.heads, C // self.heads, H * W)
        a = torch.softmax(q.transpose(2, 3) @ k / math.sqrt(C // self.heads), dim=-1)
        h = (a @ v.transpose(2, 3)).transpose(2, 3).reshape(B, C, H, W)
        return x + self.o(self.n(h))


class DownBlock(nn.Module):
    def __init__(self, cin, cout, cond_dim, attn, dropout):
        super().__init__()
        self.b1 = ResBlock(cin, cout, cond_dim, dropout)
        self.b2 = ResBlock(cout, cout, cond_dim, dropout)
        self.attn = AttnBlock(cout) if attn else None

    def forward(self, x, c):
        x = self.b1(x, c)
        x = self.b2(x, c)
        if self.attn is not None:
            x = self.attn(x)
        return x


class UpBlock(nn.Module):
    def __init__(self, cin, cout, cond_dim, attn, dropout):
        super().__init__()
        self.b1 = ResBlock(cin, cout, cond_dim, dropout)
        self.b2 = ResBlock(cout, cout, cond_dim, dropout)
        self.attn = AttnBlock(cout) if attn else None

    def forward(self, x, c):
        x = self.b1(x, c)
        x = self.b2(x, c)
        if self.attn is not None:
            x = self.attn(x)
        return x


class TimeEmb(nn.Module):
    def __init__(self, dim):
        super().__init__()
        self.mlp = nn.Sequential(nn.Linear(dim, dim * 4), nn.SiLU(), nn.Linear(dim * 4, dim))

    def forward(self, t):
        half_dim = self.mlp[0].in_features // 2
        half = t[:, None] * torch.exp(-math.log(10000) * torch.arange(0, half_dim, device=t.device) / half_dim)
        return self.mlp(torch.cat([torch.sin(half), torch.cos(half)], dim=-1))


class CondEmb(nn.Module):
    def __init__(self, cond_dim):
        super().__init__()
        n = lambda k: k + 1  # +1 for null class
        self.sub = nn.Embedding(n(len(SUB_CLASSES)), cond_dim)
        self.parent = nn.Embedding(n(len(PARENT_CLASSES)), cond_dim)
        self.relief = nn.Embedding(n(len(RELIEF_CLASSES)), cond_dim)
        self.texture = nn.Embedding(n(len(TEXTURE_CLASSES)), cond_dim)
        self.water = nn.Embedding(n(len(WATER_CLASSES)), cond_dim)

    def forward(self, y):
        return (self.sub(y[:, 0]) + self.parent(y[:, 1]) + self.relief(y[:, 2])
                + self.texture(y[:, 3]) + self.water(y[:, 4]))


class TerrainUNet(nn.Module):
    def __init__(self, in_ch=1, base=96, ch_mults=(1, 2, 4, 8), attn_res=(16, 8), heads=4,
                 dropout=0.1, cond_dim=256, time_dim=256):
        super().__init__()
        self.temb = TimeEmb(time_dim)
        self.cemb = CondEmb(cond_dim)
        self.cond_proj = nn.Sequential(nn.Linear(cond_dim, cond_dim), nn.SiLU(), nn.Linear(cond_dim, cond_dim))
        self.cond_norm = nn.LayerNorm(cond_dim)

        chs = [base * m for m in ch_mults]
        self.input = nn.Conv2d(in_ch, chs[0], 3, padding=1)
        self.downs = nn.ModuleList()
        for i in range(len(chs)):
            self.downs.append(DownBlock(chs[i - 1] if i > 0 else chs[0], chs[i], cond_dim,
                                        i >= len(chs) - 2, dropout))
        self.mid = nn.ModuleList([
            ResBlock(chs[-1], chs[-1], cond_dim, dropout),
            AttnBlock(chs[-1], heads),
            ResBlock(chs[-1], chs[-1], cond_dim, dropout),
        ])
        self.ups = nn.ModuleList()
        for i in range(len(chs) - 1):
            skip_ch = chs[len(chs) - 2 - i]
            cin = (chs[-1] if i == 0 else chs[len(chs) - 1 - i]) + skip_ch
            self.ups.append(UpBlock(cin, skip_ch, cond_dim, i == 0, dropout))
        self.ups.append(UpBlock(chs[0], chs[0], cond_dim, False, dropout))
        self.out = nn.Sequential(nn.GroupNorm(8, chs[0]), nn.SiLU(), nn.Conv2d(chs[0], in_ch, 3, padding=1))

    def forward(self, x, t, y):
        B = x.shape[0]
        te = self.temb(t)
        ce = self.cond_proj(self.cond_norm(self.cemb(y)))
        c = te + ce
        h = self.input(x)
        hs = []
        for i, d in enumerate(self.downs):
            h = d(h, c)
            hs.append(h)
            if i < len(self.downs) - 1:
                h = F.avg_pool2d(h, 2)
        h = self.mid[0](h, c)
        h = self.mid[1](h)
        h = self.mid[2](h, c)
        for i in range(len(self.ups) - 1):
            h = F.interpolate(h, scale_factor=2, mode="nearest")
            h = torch.cat([h, hs[len(self.ups) - 2 - i]], dim=1)
            h = self.ups[i](h, c)
        h = self.ups[-1](h, c)
        return self.out(h)


# ---------------- data ----------------
class TerrainData:
    def __init__(self, root, csv_path, size=256):
        with open(csv_path) as f:
            rows = list(csv.DictReader(f))
        self.tiles = [np.load(os.path.join(root, f"{r['grid_cell']}.npy")) for r in rows]
        labels = []
        for r in rows:
            labels.append([S2I[r["sub"]], P2I[r["parent"]], R2I[r["relief_class"]],
                           T2I[r["texture"]], W2I[r["water_class"]]])
        self.labels = np.array(labels, dtype=np.int64)
        self.n = len(self.tiles)
        self.size = size

    def sample(self, bs, rng):
        idx = rng.integers(0, self.n, bs)
        xs, ys = [], []
        for i in idx:
            t = self.tiles[i]
            h, w = t.shape
            size = self.size
            r = rng.integers(0, h - size + 1)
            c = rng.integers(0, w - size + 1)
            x = t[r:r + size, c:c + size].copy()
            if rng.random() < 0.5:
                x = x[:, ::-1]
            k = rng.integers(0, 4)
            x = np.rot90(x, k)
            xs.append(x)
            ys.append(self.labels[i])
        x = torch.from_numpy(np.stack(xs)[:, None]).float()
        return x, torch.from_numpy(np.stack(ys))


# ---------------- training ----------------
def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--data", default="/root/terrain/data")
    ap.add_argument("--out", default="/root/terrain/runs/run1")
    ap.add_argument("--steps", type=int, default=60000)
    ap.add_argument("--batch", type=int, default=16)
    ap.add_argument("--lr", type=float, default=1e-4)
    ap.add_argument("--T", type=int, default=1000)
    ap.add_argument("--schedule", default="cosine")
    ap.add_argument("--size", type=int, default=256)
    ap.add_argument("--base", type=int, default=96)
    ap.add_argument("--p_uncond", type=float, default=0.12)
    ap.add_argument("--seed", type=int, default=0)
    ap.add_argument("--ckpt_every", type=int, default=2000)
    args = ap.parse_args()

    random.seed(args.seed)
    np.random.seed(args.seed)
    torch.manual_seed(args.seed)
    os.makedirs(args.out, exist_ok=True)

    device = "cuda" if torch.cuda.is_available() else "cpu"
    print("device:", device, torch.cuda.get_device_name(0) if device == "cuda" else "")

    ds = TerrainData(os.path.join(args.data, "npy"), os.path.join(args.data, "labels_final.csv"), args.size)
    print(f"dataset: {ds.n} tiles, {ds.size}x{ds.size} crops")

    betas = (betas_cosine(args.T) if args.schedule == "cosine" else betas_linear(args.T)).to(device)
    alphas = 1 - betas
    alpha_bar = torch.cumprod(alphas, 0)
    sqrt_alpha_bar = torch.sqrt(alpha_bar)
    sqrt_one_minus_ab = torch.sqrt(1 - alpha_bar)

    model = TerrainUNet(base=args.base).to(device)
    ema = TerrainUNet(base=args.base).to(device)
    ema.load_state_dict(model.state_dict())
    for p in ema.parameters():
        p.requires_grad_(False)
    print(f"params: {sum(p.numel() for p in model.parameters()) / 1e6:.1f}M")

    opt = torch.optim.AdamW(model.parameters(), lr=args.lr, weight_decay=0.0)
    warmup = 500
    sched = torch.optim.lr_scheduler.CosineAnnealingLR(opt, T_max=max(1, args.steps - warmup))

    dirs = dict(os.environ)
    rng = np.random.default_rng(args.seed)
    ema_decay = 0.999
    t0 = time.time()
    step = 0
    losses = []

    def ema_update():
        with torch.no_grad():
            for p, pe in zip(model.parameters(), ema.parameters()):
                pe.mul_(ema_decay).add_(p.detach(), alpha=1 - ema_decay)
            for b, be in zip(model.buffers(), ema.buffers()):
                be.copy_(b)

    def preview(tag):
        ema.eval()
        with torch.no_grad():
            n = 4
            cond = torch.tensor([[S2I["alpine"], P2I["mountain"], R2I["extreme"], T2I["coarse_rugged"], W2I["land"]],
                                 [S2I["dune_sea"], P2I["desert"], R2I["mid"], T2I["fine_ridged"], W2I["land"]],
                                 [S2I["fjord"], P2I["coastal"], R2I["extreme"], T2I["coarse_rugged"], W2I["water_lots"]],
                                 [S2I["flat_plain"], P2I["plain"], R2I["flat"], T2I["smooth"], W2I["land"]]], device=device)
            x = torch.randn(n, 1, args.size, args.size, device=device)
            ddim_steps = 50
            for i in range(ddim_steps):
                t1 = int((args.T - 1) * (1 - i / ddim_steps))
                t0 = int((args.T - 1) * (1 - (i + 1) / ddim_steps))
                t = torch.full((n,), max(t1, 1), device=device)
                eps = ema(x, t, cond)
                x0h = (x - sqrt_one_minus_ab[t][:, None, None, None] * eps) / sqrt_alpha_bar[t][:, None, None, None]
                x0h = x0h.clamp(-3, 3)
                t0v = torch.tensor([t0], device=device)
                x = sqrt_alpha_bar[t0v][:, None, None, None] * x0h + sqrt_one_minus_ab[t0v][:, None, None, None] * eps
            grid = []
            for i in range(n):
                z = x[i, 0].cpu().numpy()
                grid.append(((z - z.min()) / (z.max() - z.min() + 1e-9)).clip(0, 1))
            from PIL import Image
            im = np.hstack([(g * 255).astype(np.uint8) for g in grid])
            Image.fromarray(im).save(os.path.join(args.out, f"prev_{tag}.png"))
        model.train()

    model.train()

    while step < args.steps:
        x, y = ds.sample(args.batch, rng)
        x, y = x.to(device), y.to(device)
        if args.p_uncond > 0:
            drop = torch.rand(x.shape[0], device=device) < args.p_uncond
            y = y.clone()
            y[drop] = torch.tensor([[len(SUB_CLASSES), len(PARENT_CLASSES), len(RELIEF_CLASSES),
                                     len(TEXTURE_CLASSES), len(WATER_CLASSES)]], device=device)
        t = torch.randint(0, args.T, (x.shape[0],), device=device)
        noise = torch.randn_like(x)
        xt = sqrt_alpha_bar[t, None, None, None] * x + sqrt_one_minus_ab[t, None, None, None] * noise
        pred = model(xt, t, y)
        loss = F.mse_loss(pred, noise)
        opt.zero_grad()
        loss.backward()
        if step < warmup:
            opt.param_groups[0]["lr"] = args.lr * (step + 1) / warmup
        opt.step()
        if step >= warmup:
            sched.step()
        ema_update()
        losses.append(loss.item())
        step += 1

        if step % 25 == 0:
            el = sum(losses[-25:]) / min(25, len(losses))
            lr = opt.param_groups[0]["lr"]
            print(f"step {step}/{args.steps} loss {el:.4f} lr {lr:.2e} "
                  f"{time.time() - t0:.0f}s ({losses[-25:] and (time.time() - t0) / step:.2f}s/step)", flush=True)
        if step % args.ckpt_every == 0:
            torch.save({"model": model.state_dict(), "ema": ema.state_dict(), "step": step,
                        "args": vars(args)}, os.path.join(args.out, f"ckpt_{step}.pt"))
            preview(step)

    torch.save({"model": model.state_dict(), "ema": ema.state_dict(), "step": step,
                "args": vars(args)}, os.path.join(args.out, "ckpt_final.pt"))
    preview("final")
    print(f"done in {(time.time() - t0) / 60:.1f} min")


if __name__ == "__main__":
    main()