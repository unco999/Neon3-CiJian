"""Convert a terrain_run1 PyTorch checkpoint into the NEONWAI1 weight pack.

The canonical tensor names are defined by Rust `TerrainUnetSpec::terrain_unet_layout`
(crates/neon-wgpu-ai/src/format.rs).  The PyTorch checkpoint differs in exactly
one naming place: the middle blocks are bare ModuleList entries (`mid.0.*`,
`mid.1.*`, `mid.2.*`) while the canonical layout groups them as `mid.0.b1.*`,
`mid.1.attn.*`, `mid.2.b1.*`.  This script applies that mapping, then verifies
that the exported key set is identical to the canonical layout key set.

Exports the EMA weights (`ck["ema"]`), matching what the training run used for
sampling.  The output is a deterministic binary file; the meta sha256 is the
SHA-256 of the concatenated little-endian f32 payloads in layout order.

Usage:
    python convert_ckpt.py --ckpt assets/ai/terrain_run1/ckpt_final.pt \
        --out assets/ai/terrain_run1/terrain_run1.pack
"""

import argparse
import hashlib
import json
import struct
import sys
from pathlib import Path

import torch

MAGIC = b"NEONWAI1"
FORMAT_VERSION = 1
DTYPE_F32 = 0
MODEL_KIND = 0
MODEL_KIND_NAME = "terrain_unet_ddim_v1"

SUB_CLASSES = ["alpine", "caldera_rim", "canyon_gorge", "cliff_coast", "delta",
               "dissected_hills", "dune_sea", "fjord", "flat_plain", "glacier_highland",
               "hamada", "high_plateau", "lava_plateau", "mesa_badlands", "mid_mountain",
               "rocky_wadi", "rolling_hills", "salt_playa", "sandy_coast", "shield_volcano",
               "stratovolcano", "tundra_lowland", "undulating_plain"]
PARENT_CLASSES = ["coastal", "desert", "glacial", "hill", "mountain", "plain",
                  "plateau", "volcanic"]
RELIEF_CLASSES = ["flat", "low", "mid", "high", "extreme"]
TEXTURE_CLASSES = ["smooth", "undulating", "fine_ridged", "coarse_rugged"]
WATER_CLASSES = ["land", "water_edge", "water_lots"]

# Extractable params, kept in one place so we can sanity-check both sides.
BASE_CH = 96
CH_MULTS = [1, 2, 4, 8]
ATTM_FROM_LEVEL = 2  # downs levels >= 2 get attention
HEADS = 4
COND_DIM = 256
TIME_DIM = 256
GROUP_GROUPS = 8
INPUT_CH = 1

MID_PREFIX_MAP = {
    "mid.0.": "mid.0.b1.",
    "mid.1.": "mid.1.attn.",
    "mid.2.": "mid.2.b1.",
}


def channels():
    return [BASE_CH * m for m in CH_MULTS]


def canonical_layout():
    """Mirror of TerrainUnetSpec::terrain_unet_layout, returning (name, dims)."""
    out = []

    def linear(name, out_c, in_c):
        out.append((f"{name}.weight", [out_c, in_c]))
        out.append((f"{name}.bias", [out_c]))

    def conv(name, out_c, in_c, k):
        out.append((f"{name}.weight", [out_c, in_c, k, k]))
        out.append((f"{name}.bias", [out_c]))

    def norm(name, c):
        out.append((f"{name}.weight", [c]))
        out.append((f"{name}.bias", [c]))

    def film(name, c):
        out.append((f"{name}.film.weight", [c * 2, COND_DIM]))
        out.append((f"{name}.film.bias", [c * 2]))

    def res_block(name, cin, cout, skip):
        norm(f"{name}.n1", cin)
        conv(f"{name}.c1", cout, cin, 3)
        norm(f"{name}.n2", cout)
        conv(f"{name}.c2", cout, cout, 3)
        film(name, cout)
        if skip:
            out.append((f"{name}.skip.weight", [cout, cin, 1, 1]))
            out.append((f"{name}.skip.bias", [cout]))

    def attn_block(name, c):
        for part in ["q", "k", "v", "o"]:
            out.append((f"{name}.{part}.weight", [c, c, 1, 1]))
            out.append((f"{name}.{part}.bias", [c]))
        norm(f"{name}.n", c)

    linear("temb.mlp.0", TIME_DIM * 4, TIME_DIM)
    linear("temb.mlp.2", TIME_DIM, TIME_DIM * 4)
    for name, rows in [
        ("cemb.sub", len(SUB_CLASSES) + 1),
        ("cemb.parent", len(PARENT_CLASSES) + 1),
        ("cemb.relief", len(RELIEF_CLASSES) + 1),
        ("cemb.texture", len(TEXTURE_CLASSES) + 1),
        ("cemb.water", len(WATER_CLASSES) + 1),
    ]:
        out.append((f"{name}.weight", [rows, COND_DIM]))
    linear("cond_proj.0", COND_DIM, COND_DIM)
    linear("cond_proj.2", COND_DIM, COND_DIM)
    norm("cond_norm", COND_DIM)
    conv("input", channels()[0], INPUT_CH, 3)

    chs = channels()
    for i, cout in enumerate(chs):
        cin = cout if i == 0 else chs[i - 1]
        res_block(f"downs.{i}.b1", cin, cout, cin != cout)
        res_block(f"downs.{i}.b2", cout, cout, False)
        if i >= ATTM_FROM_LEVEL:
            attn_block(f"downs.{i}.attn", cout)

    last = chs[-1]
    res_block("mid.0.b1", last, last, False)
    attn_block("mid.1.attn", last)
    res_block("mid.2.b1", last, last, False)

    up_cins = [chs[-1] + chs[-2], chs[-2] + chs[-3], chs[-3] + chs[-4], chs[-4]]
    for i, cin in enumerate(up_cins):
        cout = chs[2 - i] if i < 3 else chs[0]
        res_block(f"ups.{i}.b1", cin, cout, cin != cout)
        res_block(f"ups.{i}.b2", cout, cout, False)
        if i == 0:
            attn_block(f"ups.{i}.attn", cout)

    norm("out.0", chs[0])
    conv("out.2", INPUT_CH, chs[0], 3)
    return out


def map_mid_keys(keys):
    """Rewrite bare ModuleList mid keys to canonical grouped names."""
    mapped = {}
    for key in keys:
        out_key = key
        for prefix, replacement in MID_PREFIX_MAP.items():
            if key.startswith(prefix):
                out_key = replacement + key[len(prefix):]
                break
        mapped[out_key] = key
    return mapped


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ckpt", required=True, help="path to ckpt_final.pt")
    ap.add_argument("--out", required=True, help="output .pack path")
    args = ap.parse_args()

    ckpt_path = Path(args.ckpt)
    print(f"loading {ckpt_path} ...")
    ck = torch.load(ckpt_path, map_location="cpu", weights_only=False)
    if "ema" not in ck:
        sys.exit("checkpoint has no 'ema' state dict")
    ema = ck["ema"]

    layout = canonical_layout()
    expected = {name for name, _ in layout}
    mapped = map_mid_keys(ema.keys())
    actual = set(mapped.keys())

    missing = expected - actual
    extra = actual - expected
    if missing:
        sample = sorted(missing)[:10]
        sys.exit(f"checkpoint is missing canonical tensors (first 10): {sample}")
    if extra:
        sample = sorted(extra)[:10]
        sys.exit(f"checkpoint has tensors not in canonical layout (first 10): {sample}")

    for name, dims in layout:
        tensor = ema[mapped[name]]
        tensor = tensor.detach().float().contiguous()
        if list(tensor.shape) != dims:
            sys.exit(f"tensor '{name}' shape {list(tensor.shape)} != canonical {dims}")

    meta = ck.get("args") or {}
    meta_obj = {
        "model_kind": MODEL_KIND_NAME,
        "dtype": "f32",
        "T": int(meta.get("T", 1000)) if hasattr(meta, "get") else 1000,
        "base": int(meta.get("base", BASE_CH)) if hasattr(meta, "get") else BASE_CH,
        "schedule": "cosine",
        "source_ckpt": ckpt_path.name,
        "param_count": 0,
        "sha256": "",
        "created_at": "2026-08-14T00:00:00Z",
    }
    if isinstance(meta, str):
        meta_obj["T"] = 1000
        meta_obj["base"] = BASE_CH

    payload = hashlib.sha256()
    param_count = 0
    chunks = []
    for name, dims in layout:
        tensor = ema[mapped[name]].detach().float().contiguous()
        numel = tensor.numel()
        param_count += numel
        raw = tensor.numpy().astype("<f4").tobytes()
        assert len(raw) == numel * 4
        payload.update(raw)
        chunks.append((name, dims, raw))
    meta_obj["param_count"] = param_count
    meta_obj["sha256"] = payload.hexdigest()

    meta_bytes = json.dumps(meta_obj, separators=(",", ":")).encode("utf-8")

    out = bytearray()
    out += MAGIC
    out += struct.pack("<I", FORMAT_VERSION)
    out += struct.pack("<I", MODEL_KIND)
    out += struct.pack("<I", DTYPE_F32)
    out += struct.pack("<I", len(layout))
    out += struct.pack("<I", len(meta_bytes))
    out += meta_bytes
    for name, dims, raw in chunks:
        name_bytes = name.encode("utf-8")
        out += struct.pack("<I", len(name_bytes))
        out += name_bytes
        out += struct.pack("<I", len(dims))
        for d in dims:
            out += struct.pack("<I", d)
        out += struct.pack("<I", len(raw))
        out += raw

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_bytes(out)
    print(f"wrote {out_path} ({len(out) / 1_048_576:.1f} MB, {param_count} params, sha256={meta_obj['sha256']})")


if __name__ == "__main__":
    main()