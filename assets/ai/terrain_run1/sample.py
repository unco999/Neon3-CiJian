"""DDIM + CFG sampling from trained checkpoint -> 16-bit PNG heightmaps."""
import argparse
import math
import os

import numpy as np
import torch
import torch.nn as nn
import torch.nn.functional as F

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


def betas_cosine(T, s=0.008):
    steps = torch.arange(T + 1, dtype=torch.float32) / T
    alpha_bar = torch.cos((steps + s) / (1 + s) * math.pi / 2) ** 2
    return torch.clip(1 - alpha_bar[1:] / alpha_bar[:-1], max=0.999)


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
        n = lambda k: k + 1
        self.sub = nn.Embedding(n(len(SUB_CLASSES)), cond_dim)
        self.parent = nn.Embedding(n(len(PARENT_CLASSES)), cond_dim)
        self.relief = nn.Embedding(n(len(RELIEF_CLASSES)), cond_dim)
        self.texture = nn.Embedding(n(len(TEXTURE_CLASSES)), cond_dim)
        self.water = nn.Embedding(n(len(WATER_CLASSES)), cond_dim)

    def forward(self, y):
        return (self.sub(y[:, 0]) + self.parent(y[:, 1]) + self.relief(y[:, 2])
                + self.texture(y[:, 3]) + self.water(y[:, 4]))


class TerrainUNet(nn.Module):
    def __init__(self, in_ch=1, base=96, ch_mults=(1, 2, 4, 8), heads=4, dropout=0.1,
                 cond_dim=256, time_dim=256):
        super().__init__()
        import torch.nn.functional as F
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
        import torch.nn.functional as F
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


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ckpt", default="/root/terrain/runs/run1/ckpt_final.pt")
    ap.add_argument("--out", default="/root/terrain/samples")
    ap.add_argument("--n", type=int, default=8)
    ap.add_argument("--sub", default="alpine")
    ap.add_argument("--parent", default="mountain")
    ap.add_argument("--relief", default="extreme")
    ap.add_argument("--texture", default="coarse_rugged")
    ap.add_argument("--water", default="land")
    ap.add_argument("--guidance", type=float, default=3.0)
    ap.add_argument("--steps", type=int, default=50)
    ap.add_argument("--size", type=int, default=256)
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args()

    seed = args.seed if args.seed >= 0 else np.random.randint(0, 1 << 30)
    torch.manual_seed(seed)
    np.random.seed(seed)

    import torch.nn.functional as F
    device = "cuda" if torch.cuda.is_available() else "cpu"
    ck = torch.load(args.ckpt, map_location="cpu")
    T = ck["args"]["T"]
    base = ck["args"]["base"]
    betas = betas_cosine(T).to(device)
    alphas = 1 - betas
    alpha_bar = torch.cumprod(alphas, 0)
    sab = torch.sqrt(alpha_bar)
    s1ab = torch.sqrt(1 - alpha_bar)

    model = TerrainUNet(base=base).to(device)
    model.load_state_dict(ck["ema"])
    model.eval()

    cond = torch.tensor([[S2I[args.sub], P2I[args.parent], R2I[args.relief],
                          T2I[args.texture], W2I[args.water]]] * args.n, device=device)
    uncond = torch.tensor([[len(SUB_CLASSES), len(PARENT_CLASSES), len(RELIEF_CLASSES),
                            len(TEXTURE_CLASSES), len(WATER_CLASSES)]] * args.n, device=device)

    os.makedirs(args.out, exist_ok=True)
    x = torch.randn(args.n, 1, args.size, args.size, device=device)
    gw = args.guidance
    with torch.no_grad():
        for i in range(args.steps):
            t1 = int((T - 1) * (1 - i / args.steps))
            t0 = int((T - 1) * (1 - (i + 1) / args.steps))
            t = torch.full((args.n,), max(t1, 1), device=device)
            ec = model(x, t, cond)
            if gw > 0:
                eu = model(x, t, uncond)
                e = eu + gw * (ec - eu)
            else:
                e = ec
            x0h = (x - s1ab[t][:, None, None, None] * e) / sab[t][:, None, None, None]
            x0h = x0h.clamp(-3, 3)
            t0v = torch.tensor([t0], device=device)
            x = sab[t0v][:, None, None, None] * x0h + s1ab[t0v][:, None, None, None] * e

    from PIL import Image
    for i in range(args.n):
        z = x[i, 0].cpu().numpy()
        print(f"sample {i}: min={z.min():.6f} max={z.max():.6f} mean={z.mean():.6f} std={z.std():.6f}")
        z = z - z.min()
        z = z / (z.max() + 1e-9)
        u16 = (z * 65535).astype(np.uint16)
        Image.fromarray(u16).save(os.path.join(args.out, f"{args.sub}_{i:02d}.png"))
    print(f"saved {args.n} heightmaps: {args.sub} {args.relief} {args.texture} {args.water} gw={gw} seed={seed}")


if __name__ == "__main__":
    main()
