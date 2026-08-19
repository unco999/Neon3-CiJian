import struct, json, os

SRC = r"E:\game_resouce\neon2资源\模型\ultimate_monsters_pack.glb"
OUT_DIR = r"D:\bevy-nui-host\assets\monsters"
os.makedirs(OUT_DIR, exist_ok=True)

data = open(SRC, "rb").read()
magic, version, length = struct.unpack("<III", data[:12])
assert magic == 0x46546C67
off = 12
jbytes = None
bbytes = None
while off < len(data):
    clen, ctype = struct.unpack("<II", data[off:off + 8])
    off += 8
    chunk = data[off:off + clen]
    off += clen
    if ctype == 0x4E4F534A:
        jbytes = chunk
    elif ctype == 0x004E4942:
        bbytes = chunk

gltf = json.loads(jbytes.decode("utf-8"))
nodes = gltf["nodes"]
meshes = gltf["meshes"]
skins = gltf["skins"]
mats = gltf["materials"]
anims = gltf.get("animations", [])
accessors = gltf["accessors"]
bufferViews = gltf["bufferViews"]

COMP_SIZES = {5120: 1, 5121: 1, 5122: 2, 5123: 2, 5125: 4, 5126: 4}
COMP_COUNT = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT2": 4, "MAT3": 9, "MAT4": 16}

def accessor_byte_length(acc):
    comp_size = COMP_SIZES[acc["componentType"]]
    comp_count = COMP_COUNT[acc["type"]]
    elem_size = comp_count * comp_size
    stride = acc.get("byteStride", elem_size)
    return stride * (acc["count"] - 1) + elem_size

def subtree(root):
    out = []
    stack = [root]
    while stack:
        i = stack.pop()
        out.append(i)
        stack.extend(nodes[i].get("children", []))
    return out

def base_name(arm):
    for c in nodes[arm].get("children", []):
        cn = nodes[c]
        if cn.get("mesh") is None and cn.get("skin") is None and "rootJoint" not in cn.get("name", ""):
            n = cn.get("name", f"monster_{arm}")
            # strip trailing .NNN (Blender duplicate suffix)
            if "." in n:
                head, tail = n.rsplit(".", 1)
                if tail.isdigit():
                    n = head
            return n
    return f"monster_{arm}"

armatures = [
    i for i, n in enumerate(nodes)
    if n.get("name", "").startswith("CharacterArmature")
    and not n.get("name", "").endswith("rootJoint")
]

used_names = {}
results = []
for arm in armatures:
    raw = base_name(arm)
    n_dup = used_names.get(raw, 0)
    used_names[raw] = n_dup + 1
    name = raw if n_dup == 0 else f"{raw}_{n_dup}"

    node_list = subtree(arm)
    node_set = set(node_list)
    node_remap = {old: new for new, old in enumerate(node_list)}

    used_meshes = sorted({n["mesh"] for i in node_list if (n := nodes[i]).get("mesh") is not None})
    mesh_remap = {old: new for new, old in enumerate(used_meshes)}

    used_mats = set()
    for mi in used_meshes:
        for prim in meshes[mi].get("primitives", []):
            if "material" in prim:
                used_mats.add(prim["material"])
    used_mats = sorted(used_mats)
    mat_remap = {old: new for new, old in enumerate(used_mats)}

    used_skins = sorted({n["skin"] for i in node_list if (n := nodes[i]).get("skin") is not None})
    skin_remap = {old: new for new, old in enumerate(used_skins)}

    kept_accessors = set()

    # animation channels — collect WITHOUT mutating the shared source dicts.
    # Store (sampler_id, target_node, path) triples; rebuild fresh dicts later
    # after accessor_remap is known.
    anim_plan = []
    for a in anims:
        src_samplers = a.get("samplers", [])
        kept = [
            (ch["sampler"], ch["target"].get("node"), ch["target"].get("path"))
            for ch in a.get("channels", [])
            if ch["target"].get("node") in node_set
        ]
        if kept:
            anim_plan.append((src_samplers, kept))
    for src_samplers, kept in anim_plan:
        for sid, _, _ in kept:
            s = src_samplers[sid]
            for k in ("input", "output"):
                if k in s:
                    kept_accessors.add(s[k])

    # mesh + skin accessors
    for mi in used_meshes:
        for prim in meshes[mi].get("primitives", []):
            for v in prim.get("attributes", {}).values():
                kept_accessors.add(v)
            if "indices" in prim:
                kept_accessors.add(prim["indices"])
    for si in used_skins:
        if "inverseBindMatrices" in skins[si]:
            kept_accessors.add(skins[si]["inverseBindMatrices"])

    kept_accessors = sorted(kept_accessors)
    accessor_remap = {old: new for new, old in enumerate(kept_accessors)}

    # ---- build new animations with remapped sampler/node/accessor indices ----
    new_anims = []
    for src_samplers, kept in anim_plan:
        s_ids = sorted({sid for sid, _, _ in kept})
        s_remap = {old: new for new, old in enumerate(s_ids)}
        new_channels = [
            {"sampler": s_remap[sid], "target": {"node": node_remap[tnode], "path": path}}
            for sid, tnode, path in kept
        ]
        new_samplers = []
        for sid in s_ids:
            s = dict(src_samplers[sid])
            for k in ("input", "output"):
                if k in s:
                    s[k] = accessor_remap[s[k]]
            new_samplers.append(s)
        new_anims.append({"name": "idle", "channels": new_channels, "samplers": new_samplers})

    # accessor-level buffer slicing (one bufferView per accessor)
    new_buffer = bytearray()
    new_bvs = []
    new_accessors = []
    for old_a in kept_accessors:
        acc = dict(accessors[old_a])
        if "bufferView" in acc:
            bv = bufferViews[acc["bufferView"]]
            start = bv.get("byteOffset", 0) + acc.get("byteOffset", 0)
            ln = accessor_byte_length(accessors[old_a])
            pad = (-len(new_buffer)) % 4
            new_buffer += b"\x00" * pad
            new_offset = len(new_buffer)
            new_buffer += bbytes[start:start + ln]
            acc["bufferView"] = len(new_bvs)
            acc["byteOffset"] = 0
            new_bvs.append({"buffer": 0, "byteOffset": new_offset, "byteLength": ln})
        new_accessors.append(acc)

    # nodes
    new_nodes = []
    for i in node_list:
        n = dict(nodes[i])
        if "children" in n:
            n["children"] = [node_remap[c] for c in n["children"]]
        if "mesh" in n:
            n["mesh"] = mesh_remap[n["mesh"]]
        if "skin" in n:
            n["skin"] = skin_remap[n["skin"]]
        if i == arm:
            n.pop("matrix", None)
            n["translation"] = [0.0, 0.0, 0.0]
            n["rotation"] = [0.0, 0.0, 0.0, 1.0]
        new_nodes.append(n)

    # meshes
    new_meshes = []
    for old_m in used_meshes:
        m = dict(meshes[old_m])
        prims = []
        for prim in m.get("primitives", []):
            np = dict(prim)
            np["attributes"] = {k: accessor_remap[v] for k, v in prim.get("attributes", {}).items()}
            if "indices" in np:
                np["indices"] = accessor_remap[np["indices"]]
            if "material" in np:
                np["material"] = mat_remap[np["material"]]
            prims.append(np)
        m["primitives"] = prims
        new_meshes.append(m)

    new_mats = [dict(mats[o]) for o in used_mats]

    new_skins = []
    for old_s in used_skins:
        s = dict(skins[old_s])
        s["joints"] = [node_remap[j] for j in s["joints"]]
        if "skeleton" in s:
            s["skeleton"] = node_remap[s["skeleton"]]
        if "inverseBindMatrices" in s:
            s["inverseBindMatrices"] = accessor_remap[s["inverseBindMatrices"]]
        new_skins.append(s)

    out_gltf = {
        "asset": {"version": "2.0", "generator": "neon3-splitter"},
        "scene": 0,
        "scenes": [{"nodes": [0], "name": name}],
        "nodes": new_nodes,
        "meshes": new_meshes,
        "materials": new_mats,
        "skins": new_skins,
        "animations": new_anims,
        "accessors": new_accessors,
        "bufferViews": new_bvs,
        "buffers": [{"byteLength": len(new_buffer)}],
    }

    json_bytes = json.dumps(out_gltf, separators=(",", ":")).encode("utf-8")
    while len(json_bytes) % 4 != 0:
        json_bytes += b" "
    bin_out = bytes(new_buffer)
    while len(bin_out) % 4 != 0:
        bin_out += b"\x00"

    def chunk(ctype, payload):
        return struct.pack("<II", len(payload), ctype) + payload

    header = struct.pack("<III", 0x46546C67, 2, 12 + 8 + len(json_bytes) + 8 + len(bin_out))
    glb = header + chunk(0x4E4F534A, json_bytes) + chunk(0x004E4942, bin_out)

    out_path = os.path.join(OUT_DIR, name + ".glb")
    with open(out_path, "wb") as f:
        f.write(glb)
    results.append((name, len(new_nodes), len(new_meshes), len(new_anims), len(glb)))

print(f"split {len(results)} monsters:")
for name, nn, nm, na, size in results:
    print(f"  {name:24s} nodes={nn:4d} meshes={nm:3d} anims={na}  {size//1024}KB")
