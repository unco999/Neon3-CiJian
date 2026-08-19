import struct, json

path = r"E:\game_resouce\neon2资源\模型\ultimate_monsters_pack.glb"
with open(path, "rb") as f:
    data = f.read()
magic, version, length = struct.unpack("<III", data[:12])
off = 12
json_data = None
while off < len(data):
    clen, ctype = struct.unpack("<II", data[off:off + 8])
    off += 8
    chunk = data[off:off + clen]
    off += clen
    if ctype == 0x4E4F534A:
        json_data = chunk
gltf = json.loads(json_data.decode("utf-8"))
nodes = gltf["nodes"]
anims = gltf.get("animations", [])
skins = gltf.get("skins", [])

# List all armature (monster) roots: nodes whose name starts with "CharacterArmature"
armatures = [(i, n) for i, n in enumerate(nodes) if n.get("name", "").startswith("CharacterArmature")]
print(f"=== {len(armatures)} CharacterArmature nodes ===")
for i, n in armatures:
    # mesh children under this armature
    mesh_kids = []
    for c in n.get("children", []):
        cn = nodes[c]
        if cn.get("mesh") is not None and cn.get("skin") is not None:
            mesh_kids.append(cn.get("name"))
    # base monster name = first mesh kid prefix before "_0"
    base = None
    for m in mesh_kids:
        b = m.rsplit("_", 1)[0].rstrip(".0123456789")
        base = b
        break
    print(f"  armature[{i}] {n['name']!r}  meshes={mesh_kids}  base={base!r}")

# Distinct base monster types
bases = set()
for i, n in armatures:
    for c in n.get("children", []):
        cn = nodes[c]
        if cn.get("mesh") is not None and cn.get("skin") is not None:
            b = cn["name"].rsplit("_", 1)[0]
            # strip trailing .NNN
            while b and (b[-1].isdigit() or b[-1] == "."):
                b = b[:-1]
            bases.add(b)
            break
print(f"\n=== {len(bases)} distinct monster base names ===")
for b in sorted(bases):
    print(f"  {b!r}")

# Animation analysis
print(f"\n=== animation(s): {len(anims)} ===")
for ai, a in enumerate(anims):
    chans = a.get("channels", [])
    # which nodes are animated
    animated_nodes = {}
    for ch in chans:
        t = ch.get("target", {})
        nid = t.get("node")
        animated_nodes.setdefault(nid, []).append(t.get("path"))
    print(f"  anim[{ai}] {a.get('name','?')!r}: {len(chans)} channels, {len(animated_nodes)} distinct nodes animated")
    # sample a few
    sample = list(animated_nodes.items())[:8]
    for nid, paths in sample:
        print(f"      node[{nid}] {nodes[nid].get('name')!r} paths={paths[:4]}")

# skins -> skeleton joint root
print(f"\n=== skins sample (first 5) ===")
for s in skins[:5]:
    print(f"  skin joints={len(s.get('joints',[]))} skeleton={s.get('skeleton')} mesh_inverse_bindings={len(s.get('inverseBindMatrices',[]))}")
