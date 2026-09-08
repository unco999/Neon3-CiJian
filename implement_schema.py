path = r'D:\Neon3\crates\neon-ui-schema\src\lib.rs'
with open(path, 'r', encoding='utf-8') as f:
    content = f.read()

# 1. Add UiCompositionLayer enum after UiNode struct
ui_node_end = '''    pub children: Vec<UiNode>,
}

/// A finite renderer-owned visual recipe for one standard control type.'''

composition_layer_enum = '''    pub children: Vec<UiNode>,
}

/// Composition destination for a node subtree. `Normal` is the legacy path,
/// `BehindGlass` is rendered into the independent surface sampled by the
/// backdrop effect, and `Top` is rendered sharply after the backdrop chain.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiCompositionLayer {
    #[default]
    Normal,
    BehindGlass,
    Top,
}

/// A finite renderer-owned visual recipe for one standard control type.'''

if ui_node_end in content:
    content = content.replace(ui_node_end, composition_layer_enum, 1)
    print("OK: added UiCompositionLayer enum")
else:
    print("MISS: UiNode end not found")

# 2. Add CompositionLayer variant to UiEffect (after Material variant)
material_effect = '''    Material {
        node_id: UiNodeId,
        material: UiMaterialRef,
    },'''

composition_effect = '''    Material {
        node_id: UiNodeId,
        material: UiMaterialRef,
    },
    /// Routes a node and all descendants to one of the window composition
    /// layers. The renderer applies inheritance from the declared node.
    CompositionLayer {
        node_id: UiNodeId,
        #[serde(default)]
        layer: UiCompositionLayer,
    },'''

if material_effect in content:
    content = content.replace(material_effect, composition_effect, 1)
    print("OK: added CompositionLayer effect")
else:
    print("MISS: Material effect not found")

# 3. Add composition_layer_records to UiIrDocument (after material_records)
ir_material = '''    #[serde(default)]
    pub material_records: std::collections::BTreeMap<String, UiMaterialRef>,'''

ir_composition = '''    #[serde(default)]
    pub material_records: std::collections::BTreeMap<String, UiMaterialRef>,
    /// Node key to composition destination. Missing entries are `normal`.
    #[serde(default)]
    pub composition_layer_records: std::collections::BTreeMap<String, UiCompositionLayer>,'''

if ir_material in content:
    content = content.replace(ir_material, ir_composition, 1)
    print("OK: added composition_layer_records to UiIrDocument")
else:
    print("MISS: UiIrDocument material_records not found")

# 4. Add composition_layer_records to UiProgram (after material_records)
program_material = '''    /// Graphical node key to material reference.
    #[serde(default)]
    pub material_records: std::collections::BTreeMap<String, UiMaterialRef>,'''

program_composition = '''    /// Graphical node key to material reference.
    #[serde(default)]
    pub material_records: std::collections::BTreeMap<String, UiMaterialRef>,
    #[serde(default)]
    pub composition_layer_records: std::collections::BTreeMap<String, UiCompositionLayer>,'''

if program_material in content:
    content = content.replace(program_material, program_composition, 1)
    print("OK: added composition_layer_records to UiProgram")
else:
    print("MISS: UiProgram material_records not found")

# 5. Add validation for CompositionLayer effect (after Material validation)
material_validate = '''            Self::Material { node_id, material } => {
                if node_id.0.trim().is_empty() {
                    Err(UiSchemaError::InvalidProgramEvent)
                } else {
                    material.validate()
                }
            }'''

composition_validate = '''            Self::Material { node_id, material } => {
                if node_id.0.trim().is_empty() {
                    Err(UiSchemaError::InvalidProgramEvent)
                } else {
                    material.validate()
                }
            }
            Self::CompositionLayer { node_id, .. } => {
                if node_id.0.trim().is_empty() {
                    Err(UiSchemaError::InvalidProgramEvent)
                } else {
                    Ok(())
                }
            }'''

if material_validate in content:
    content = content.replace(material_validate, composition_validate, 1)
    print("OK: added CompositionLayer validation")
else:
    print("MISS: Material validation not found")

with open(path, 'w', encoding='utf-8') as f:
    f.write(content)
print("schema implemented")
