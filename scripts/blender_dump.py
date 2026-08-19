import bpy
import sys

src = r"E:\game_resouce\neon2资源\模型\ultimate_monsters_pack.glb"

# fresh scene
bpy.ops.wm.read_factory_settings(use_empty=True)

before = set(bpy.data.objects)
bpy.ops.import_scene.gltf(filepath=src)
new_objs = [o for o in bpy.data.objects if o.name not in before]

armatures = [o for o in new_objs if o.type == 'ARMATURE']
meshes = [o for o in new_objs if o.type == 'MESH']

print(f"imported: {len(new_objs)} objects, {len(armatures)} armatures, {len(meshes)} meshes")

# actions
print(f"\n=== actions ({len(bpy.data.actions)}) ===")
for a in bpy.data.actions:
    print(f"  action {a.name!r}: fcurves={len(a.fcurves)} groups={len(a.groups)} frame_range={a.frame_range[:]}")

# armature names + bone counts + their actions
print(f"\n=== armatures ===")
for ar in armatures:
    bones = list(ar.data.bones)
    anim = ar.animation_data
    action = anim.action.name if anim and anim.action else None
    print(f"  {ar.name!r} bones={len(bones)} action={action!r}")

# sample bone names for first two armatures
for ar in armatures[:2]:
    print(f"\n  bones of {ar.name!r}: {[b.name for b in ar.data.bones][:8]}")

# where are the meshes parented
print("\n=== mesh parents (sample) ===")
for m in meshes[:10]:
    print(f"  mesh {m.name!r} parent={m.parent.name if m.parent else None}")

# action -> which armature's bones it drives (check fcurve data_path prefixes)
print("\n=== action fcurve data_path sample ===")
for a in bpy.data.actions[:1]:
    dps = set()
    for fc in a.fcurves:
        # data_path like pose.bones["Body"].location
        dps.add(fc.data_path)
    for dp in sorted(dps)[:20]:
        print(f"    {dp}")
    print(f"    ... total {len(dps)} distinct data_paths")
