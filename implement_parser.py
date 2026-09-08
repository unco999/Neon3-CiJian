path = r'D:\Neon3\crates\neon-ui-runtime\src\nui_flow.rs'
with open(path, 'r', encoding='utf-8') as f:
    content = f.read()

# 1. Add UiCompositionLayer to imports
old_import = '''    UiNode, UiNodeId, UiNodeKind, UiProgram, UiProgramEventDeclaration, UiProgramRevision,'''
new_import = '''    UiCompositionLayer, UiNode, UiNodeId, UiNodeKind, UiProgram, UiProgramEventDeclaration, UiProgramRevision,'''
if old_import in content:
    content = content.replace(old_import, new_import, 1)
    print("OK: added import")
else:
    print("MISS: import not found")

# 2. Add composition_layer_records to parse_nui_flow state
old_state = '''    let mut material_records = BTreeMap::new();
    let mut skins = Vec::new();'''
new_state = '''    let mut material_records = BTreeMap::new();
    let mut composition_layer_records = BTreeMap::new();
    let mut skins = Vec::new();'''
if old_state in content:
    content = content.replace(old_state, new_state, 1)
    print("OK: added parse state")
else:
    print("MISS: parse state not found")

# 3. Pass composition_layer_records to parse_and_attach calls (two places)
# First call in surface block
old_attach1 = '''                &mut root,
                &mut geometry_records,
                &mut material_records,
                line,
            )?;'''
new_attach1 = '''                &mut root,
                &mut geometry_records,
                &mut material_records,
                &mut composition_layer_records,
                line,
            )?;'''
count1 = content.count(old_attach1)
if count1 > 0:
    content = content.replace(old_attach1, new_attach1)
    print(f"OK: updated {count1} attach calls")
else:
    print("MISS: attach call not found")

# 4. Add composition_layer_records to root extraction
old_root = '''    if let Some(material) = root.material.take() {
        material_records.insert(root.node.node_id.0.clone(), material);
    }
    let mut offset = 0;'''
new_root = '''    if let Some(material) = root.material.take() {
        material_records.insert(root.node.node_id.0.clone(), material);
    }
    if root.composition_layer != UiCompositionLayer::Normal {
        composition_layer_records.insert(root.node.node_id.0.clone(), root.composition_layer);
    }
    let mut offset = 0;'''
if old_root in content:
    content = content.replace(old_root, new_root, 1)
    print("OK: added root composition layer")
else:
    print("MISS: root material not found")

# 5. Add composition_layer_records to NuiFlowDocument construction
old_doc = '''        geometry_records,
        material_records,
        branches,'''
new_doc = '''        geometry_records,
        material_records,
        composition_layer_records,
        branches,'''
if old_doc in content:
    content = content.replace(old_doc, new_doc, 1)
    print("OK: added to document")
else:
    print("MISS: document construction not found")

# 6. Add CompositionLayer effects to lower_nui_flow_effects
old_lower = '''    effects.extend(document.ir.skins.iter().cloned().map(|skin| UiEffect::ControlSkin { skin }));'''
new_lower = '''    effects.extend(document.ir.composition_layer_records.iter().map(|(node_key, layer)| {
        UiEffect::CompositionLayer {
            node_id: UiNodeId(node_key.clone()),
            layer: *layer,
        }
    }));
    effects.extend(document.ir.skins.iter().cloned().map(|skin| UiEffect::ControlSkin { skin }));'''
if old_lower in content:
    content = content.replace(old_lower, new_lower, 1)
    print("OK: added lower effects")
else:
    print("MISS: lower effects not found")

# 7. Add composition_layer to NodeBuild struct
old_nodebuild = '''    skin_key: Option<String>,
    geometry: Option<UiGeometry>,
    material: Option<UiMaterialRef>,
}'''
new_nodebuild = '''    skin_key: Option<String>,
    geometry: Option<UiGeometry>,
    material: Option<UiMaterialRef>,
    composition_layer: UiCompositionLayer,
}'''
if old_nodebuild in content:
    content = content.replace(old_nodebuild, new_nodebuild, 1)
    print("OK: added to NodeBuild")
else:
    print("MISS: NodeBuild not found")

# 8. Add composition_layer initialization in parse_node
old_parse_init = '''    let geometry = None;
    let material = None;
    let mut world_camera = None;'''
new_parse_init = '''    let geometry = None;
    let material = None;
    let mut composition_layer = UiCompositionLayer::Normal;
    let mut world_camera = None;'''
if old_parse_init in content:
    content = content.replace(old_parse_init, new_parse_init, 1)
    print("OK: added parse init")
else:
    print("MISS: parse init not found")

# 9. Add composition_layer/layer to known tokens
old_tokens = '''            | "event" | "token" | "align" | "clip" | "fit" | "justify" | "data" | "rich" | "skin" => {'''
new_tokens = '''            | "event" | "token" | "align" | "clip" | "fit" | "justify" | "data" | "rich" | "skin"
            | "composition_layer" | "layer" => {'''
if old_tokens in content:
    content = content.replace(old_tokens, new_tokens, 1)
    print("OK: added tokens")
else:
    print("MISS: tokens not found")

# 10. Add composition_layer parsing after canvas data binding
old_canvas = '''                    bindings.push((UiBoundProperty::CanvasData, key.into()));
                } else {'''
new_canvas = '''                    bindings.push((UiBoundProperty::CanvasData, key.into()));
                } else if matches!(token, "composition_layer" | "layer") {
                    composition_layer = match (token, value) {
                        ("composition_layer", "behind_glass") | ("layer", "behind_glass") => UiCompositionLayer::BehindGlass,
                        ("composition_layer", "overlay") | ("layer", "top") => UiCompositionLayer::Top,
                        _ => return Err(error(
                            "nui_flow_invalid_composition_layer",
                            "composition layer must be behind_glass, overlay, or top",
                            line,
                            1,
                        )),
                    };
                } else {'''
if old_canvas in content:
    content = content.replace(old_canvas, new_canvas, 1)
    print("OK: added parsing")
else:
    print("MISS: canvas binding not found")

# 11. Add composition_layer to NodeBuild return
old_return = '''        skin_key,
        geometry,
        material,
    })
}'''
new_return = '''        skin_key,
        geometry,
        material,
        composition_layer,
    })
}'''
if old_return in content:
    content = content.replace(old_return, new_return, 1)
    print("OK: added to return")
else:
    print("MISS: return not found")

# 12. Update attach function signature and body
old_attach_sig = '''fn attach(
    child: NodeBuild,
    parent_id: &str,
    stack: &mut Vec<(String, NodeBuild)>,
    root: &mut Option<NodeBuild>,
    geometry_records: &mut BTreeMap<String, UiGeometry>,
    material_records: &mut BTreeMap<String, UiMaterialRef>,
    line: u32,
) -> FlowResult<()> {'''
new_attach_sig = '''fn attach(
    child: NodeBuild,
    parent_id: &str,
    stack: &mut Vec<(String, NodeBuild)>,
    root: &mut Option<NodeBuild>,
    geometry_records: &mut BTreeMap<String, UiGeometry>,
    material_records: &mut BTreeMap<String, UiMaterialRef>,
    composition_layer_records: &mut BTreeMap<String, UiCompositionLayer>,
    line: u32,
) -> FlowResult<()> {'''
if old_attach_sig in content:
    content = content.replace(old_attach_sig, new_attach_sig, 1)
    print("OK: updated attach signature")
else:
    print("MISS: attach signature not found")

# 13. Add composition_layer extraction in attach body
old_attach_body = '''    if let Some(material) = child.material.take() {
        material_records.insert(child.node.node_id.0.clone(), material);
    }
    if let Some((_, parent)) = stack.last_mut() {'''
new_attach_body = '''    if let Some(material) = child.material.take() {
        material_records.insert(child.node.node_id.0.clone(), material);
    }
    if child.composition_layer != UiCompositionLayer::Normal {
        composition_layer_records.insert(child.node.node_id.0.clone(), child.composition_layer);
    }
    if let Some((_, parent)) = stack.last_mut() {'''
if old_attach_body in content:
    content = content.replace(old_attach_body, new_attach_body, 1)
    print("OK: added attach body")
else:
    print("MISS: attach body not found")

# 14. Update format_node signature
old_format_sig = '''fn format_node(
    node: &UiNode,
    skin_references: &BTreeMap<String, String>,
    geometry_records: &BTreeMap<String, UiGeometry>,
    material_records: &BTreeMap<String, UiMaterialRef>,
    lines: &mut Vec<String>,
) {'''
new_format_sig = '''fn format_node(
    node: &UiNode,
    skin_references: &BTreeMap<String, String>,
    geometry_records: &BTreeMap<String, UiGeometry>,
    material_records: &BTreeMap<String, UiMaterialRef>,
    composition_layer_records: &BTreeMap<String, UiCompositionLayer>,
    lines: &mut Vec<String>,
) {'''
if old_format_sig in content:
    content = content.replace(old_format_sig, new_format_sig, 1)
    print("OK: updated format signature")
else:
    print("MISS: format signature not found")

# 15. Add composition_layer output in format_node (after fit)
old_format_fit = '''            line.push_str(&format!(" fit {fit}"));
        }
    }
    if let Some(TextRef::Literal { value }) = &node.text {'''
new_format_fit = '''            line.push_str(&format!(" fit {fit}"));
        }
    }
    if let Some(layer) = composition_layer_records.get(&node.node_id.0) {
        line.push_str(match layer {
            UiCompositionLayer::BehindGlass => " composition_layer behind_glass",
            UiCompositionLayer::Top => " composition_layer overlay",
            UiCompositionLayer::Normal => "",
        });
    }
    if let Some(TextRef::Literal { value }) = &node.text {'''
if old_format_fit in content:
    content = content.replace(old_format_fit, new_format_fit, 1)
    print("OK: added format output")
else:
    print("MISS: format fit not found")

# 16. Update recursive format_node call
old_format_recursive = '''            skin_references,
            geometry_records,
            material_records,
            lines,
        );'''
new_format_recursive = '''            skin_references,
            geometry_records,
            material_records,
            composition_layer_records,
            lines,
        );'''
count_recursive = content.count(old_format_recursive)
if count_recursive > 0:
    content = content.replace(old_format_recursive, new_format_recursive)
    print(f"OK: updated {count_recursive} recursive format calls")
else:
    print("MISS: recursive format call not found")

# 17. Update format_nui_flow call to format_node
old_format_call = '''        &parsed.ir.skin_references,
        &parsed.ir.geometry_records,
        &parsed.ir.material_records,
        &mut lines,
    );'''
new_format_call = '''        &parsed.ir.skin_references,
        &parsed.ir.geometry_records,
        &parsed.ir.material_records,
        &parsed.ir.composition_layer_records,
        &mut lines,
    );'''
if old_format_call in content:
    content = content.replace(old_format_call, new_format_call, 1)
    print("OK: updated format_nui_flow call")
else:
    print("MISS: format_nui_flow call not found")

with open(path, 'w', encoding='utf-8') as f:
    f.write(content)
print("parser implemented")
