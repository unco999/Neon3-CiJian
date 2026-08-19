import struct, json, sys

path = r"E:\game_resouce\neon2资源\模型\ultimate_monsters_pack.glb"
with open(path, "rb") as f:
    data = f.read()

magic, version, length = struct.unpack("<III", data[:12])
assert magic == 0x46546C67, "not glTF"
print(f"GLB v{version}, total {length} bytes")

off = 12
json_data = None
while off < len(data):
    clen, ctype = struct.unpack("<II", data[off:off + 8])
    off += 8
    chunk = data[off:off + clen]
    off += clen
    if ctype == 0x4E4F534A:
        json_data = chunk
    elif ctype == 0x004E4942:
        pass

gltf = json.loads(json_data.decode("utf-8"))
print("top-level keys:", list(gltf.keys()))

nodes = gltf.get("nodes", [])
meshes = gltf.get("meshes", [])
skins = gltf.get("skins", [])
anims = gltf.get("animations", [])
mats = gltf.get("materials", [])

print(f"\ncounts: nodes={len(nodes)} meshes={len(meshes)} skins={len(skins)} animations={len(anims)} materials={len(mats)}")

# Build parent map
parent = {}
for i, n in enumerate(nodes):
    for c in n.get("children", []):
        parent[c] = i

# Root nodes (no parent)
roots = [i for i in range(len(nodes)) if i not in parent]
print(f"\nroot nodes: {len(roots)}")

def name_of(i):
    n = nodes[i]
    return n.get("name") or f"node_{i}"

print("\n=== ROOT NODES ===")
for r in roots:
    n = nodes[r]
    kids = n.get("children", [])
    print(f"  [{r}] {name_of(r)!r} mesh={n.get('mesh')} children={len(kids)}")

# Print full tree (2 levels) to understand grouping
print("\n=== TREE (roots + 2 levels) ===")
for r in roots:
    def dump(i, depth):
        n = nodes[i]
        print("  " + "  " * depth + f"[{i}] {name_of(i)!r} mesh={n.get('mesh')} skin={n.get('skin')}")
        for c in n.get("children", []):
            dump(c, depth + 1)
    dump(r, 0)

print("\n=== ANIMATIONS ===")
for i, a in enumerate(anims):
    chans = a.get("channels", [])
    targets = set()
    for ch in chans:
        t = ch.get("target", {})
        targets.add((t.get("node"), t.get("path")))
    print(f"  anim[{i}] {a.get('name','?')!r} channels={len(chans)} nodes_involved={len({t[0] for t in targets})}")

# Names of all nodes that look like monster roots
print("\n=== distinct node name prefixes ===")
from collections import Counter
names = [n.get("name", "") for n in nodes if n.get("name")]
c = Counter(names)
for name, cnt in c.most_common():
    print(f"  {name!r} x{cnt}")
