//! Compile-time Input Impact Graph construction.
//!
//! The impact graph is immutable after compilation: an input key change always
//! resolves to the same binding/branch/node/domain consequence set regardless
//! of runtime values. Derived-slot dependencies are closed transitively here
//! so a change to a source input also carries every impact of the slots
//! computed from it.

use std::collections::{BTreeMap, BTreeSet};

use neon_ui_schema::{
    UiBinding, UiBindingImpact, UiBranchPredicate, UiBranchRecord, UiInputImpact, UiInputSchema,
    UiInteractionImpact, UiInteractionKind, UiInvalidationDomain, UiNodeKind, UiProgramNode,
    sort_dedup_domains,
};

use neon_ui_schema::UiInvalidationDomain::*;

/// A predicate input change regenerates the whole branch subtree plus the
/// layout ancestors that absorbed or released it.
const BRANCH_DOMAINS: [UiInvalidationDomain; 5] =
    [NodeState, Layout, ColorInstances, DepthInstances, HitTarget];

pub(crate) fn build_input_impacts(
    schema: &UiInputSchema,
    bindings: &[UiBinding],
    branch_records: &[UiBranchRecord],
    nodes: &[UiProgramNode],
) -> BTreeMap<String, UiInputImpact> {
    let parents: BTreeMap<&str, Option<&str>> = nodes
        .iter()
        .map(|node| (node.key.as_str(), node.parent_key.as_deref()))
        .collect();
    let mut direct: BTreeMap<String, UiInputImpact> = BTreeMap::new();
    for slot in &schema.slots {
        direct.insert(
            slot.key.clone(),
            direct_impact(&slot.key, bindings, branch_records, &parents),
        );
    }
    // A derived slot's value changes exactly when one of its referenced slots
    // changes, so every source slot inherits the derived slot's consequences.
    let derived_sources: BTreeMap<&str, Vec<String>> = schema
        .slots
        .iter()
        .filter_map(|slot| {
            let expression = slot.derived_expression.as_deref()?;
            let sources = expression
                .split_whitespace()
                .filter_map(|token| token.strip_prefix('$'))
                .filter(|key| schema.slots.iter().any(|slot| slot.key == **key))
                .map(str::to_owned)
                .collect::<Vec<_>>();
            (!sources.is_empty()).then_some((slot.key.as_str(), sources))
        })
        .collect();
    // Chained derivations converge in at most one pass per link; the bound
    // keeps a malformed cycle from looping forever.
    for _ in 0..=schema.slots.len() {
        let mut changed = false;
        for (derived_key, sources) in &derived_sources {
            let template = direct
                .get(*derived_key)
                .cloned()
                .expect("every slot has a direct impact record");
            for source in sources {
                let target = direct
                    .get_mut(source)
                    .expect("every referenced source is a declared slot");
                if merge(target, &template) {
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    for impact in direct.values_mut() {
        normalize(impact);
    }
    direct
}

fn direct_impact(
    input_key: &str,
    bindings: &[UiBinding],
    branch_records: &[UiBranchRecord],
    parents: &BTreeMap<&str, Option<&str>>,
) -> UiInputImpact {
    let mut binding_impacts = Vec::new();
    let mut binding_ids = Vec::new();
    let mut affected: BTreeSet<String> = BTreeSet::new();
    let mut domains: Vec<UiInvalidationDomain> = Vec::new();
    for binding in bindings
        .iter()
        .filter(|binding| binding.input_key == input_key)
    {
        let property_domains = binding.property.invalidation_domains();
        domains.extend(property_domains.iter().cloned());
        binding_ids.push(binding.binding_id);
        affected.insert(binding.node_key.clone());
        if binding.property == neon_ui_schema::UiBoundProperty::Visible {
            for ancestor in layout_ancestors(&binding.node_key, parents) {
                affected.insert(ancestor);
            }
        }
        binding_impacts.push(UiBindingImpact {
            binding_id: binding.binding_id,
            node_key: binding.node_key.clone(),
            property: binding.property.clone(),
            domains: property_domains,
        });
    }
    let branch_keys = branch_records
        .iter()
        .filter(|branch| branch_predicate_input(&branch.predicate) == Some(input_key))
        .map(|branch| branch.branch_key.clone())
        .collect::<Vec<_>>();
    if !branch_keys.is_empty() {
        domains.extend(BRANCH_DOMAINS.iter().cloned());
        for branch in branch_records
            .iter()
            .filter(|branch| branch_predicate_input(&branch.predicate) == Some(input_key))
        {
            for node_key in &branch.node_range {
                affected.insert(node_key.clone());
                for ancestor in layout_ancestors(node_key, parents) {
                    affected.insert(ancestor);
                }
            }
        }
    }
    UiInputImpact {
        input_key: input_key.to_owned(),
        binding_impacts,
        binding_ids,
        branch_keys,
        affected_node_keys: affected.into_iter().collect(),
        domains,
    }
}

fn branch_predicate_input(branch: &UiBranchPredicate) -> Option<&str> {
    match branch {
        UiBranchPredicate::Bool { input_key, .. }
        | UiBranchPredicate::EnumEquals { input_key, .. } => Some(input_key),
        UiBranchPredicate::MachineState { .. } => None,
    }
}

fn layout_ancestors<'a>(
    node_key: &str,
    parents: &'a BTreeMap<&'a str, Option<&'a str>>,
) -> impl Iterator<Item = String> + 'a {
    let mut current = parents.get(node_key).copied().flatten();
    std::iter::from_fn(move || {
        let key = current?;
        current = parents.get(key).copied().flatten();
        Some(key.to_owned())
    })
}

/// Union `source` into `target`; returns whether anything actually changed.
fn merge(target: &mut UiInputImpact, source: &UiInputImpact) -> bool {
    let mut changed = false;
    for binding in &source.binding_impacts {
        if !target
            .binding_impacts
            .iter()
            .any(|existing| existing.binding_id == binding.binding_id)
        {
            target.binding_impacts.push(binding.clone());
            changed = true;
        }
    }
    let binding_ids: BTreeSet<u32> = target
        .binding_ids
        .iter()
        .chain(source.binding_ids.iter())
        .copied()
        .collect();
    if binding_ids.len() != target.binding_ids.len() {
        changed = true;
    }
    target.binding_ids = binding_ids.into_iter().collect();
    let branch_keys: BTreeSet<&str> = target
        .branch_keys
        .iter()
        .chain(source.branch_keys.iter())
        .map(String::as_str)
        .collect();
    if branch_keys.len() != target.branch_keys.len() {
        changed = true;
    }
    target.branch_keys = branch_keys.into_iter().map(str::to_owned).collect();
    let affected: BTreeSet<&str> = target
        .affected_node_keys
        .iter()
        .chain(source.affected_node_keys.iter())
        .map(String::as_str)
        .collect();
    if affected.len() != target.affected_node_keys.len() {
        changed = true;
    }
    target.affected_node_keys = affected.into_iter().map(str::to_owned).collect();
    let before = target.domains.len();
    target.domains.extend(source.domains.iter().cloned());
    sort_dedup_domains(&mut target.domains);
    if target.domains.len() != before {
        changed = true;
    }
    changed
}

fn normalize(impact: &mut UiInputImpact) {
    impact
        .binding_impacts
        .sort_by_key(|binding| binding.binding_id);
    impact
        .binding_impacts
        .dedup_by_key(|binding| binding.binding_id);
    impact.binding_ids.sort_unstable();
    impact.binding_ids.dedup();
    impact.branch_keys.sort();
    impact.branch_keys.dedup();
    impact.affected_node_keys.sort();
    impact.affected_node_keys.dedup();
    sort_dedup_domains(&mut impact.domains);
}

pub(crate) fn build_interaction_impacts(
    nodes: &[UiProgramNode],
    bindings: &[UiBinding],
    events: &[neon_ui_schema::UiProgramEventDeclaration],
) -> BTreeMap<String, UiInteractionImpact> {
    let mut impacts = BTreeMap::new();
    for node in nodes {
        let Some(kinds) = interaction_kinds_for(&node.kind) else {
            continue;
        };
        let mut semantic_intents: BTreeSet<&str> = BTreeSet::new();
        let mut controlled_input_keys: BTreeSet<String> = bindings
            .iter()
            .filter(|binding| binding.node_key == node.key)
            .map(|binding| binding.input_key.clone())
            .collect();
        for event in events.iter().filter(|event| event.node_key == node.key) {
            semantic_intents.insert(event.intent.as_str());
            controlled_input_keys.extend(event.bound_input_keys.iter().cloned());
        }
        impacts.insert(
            node.key.clone(),
            UiInteractionImpact {
                node_key: node.key.clone(),
                interaction_kinds: kinds.to_vec(),
                preview_domains: preview_domains_for(kinds),
                semantic_intents: semantic_intents.into_iter().map(str::to_owned).collect(),
                controlled_input_keys: controlled_input_keys.into_iter().collect(),
            },
        );
    }
    impacts
}

fn interaction_kinds_for(kind: &UiNodeKind) -> Option<&'static [UiInteractionKind]> {
    use UiInteractionKind::*;
    Some(match kind {
        UiNodeKind::Button => &[Hover, Pressed, Focus],
        UiNodeKind::Checkbox
        | UiNodeKind::Switch
        | UiNodeKind::RadioButton
        | UiNodeKind::Selectable => &[Hover, Pressed, Focus, TogglePreview],
        UiNodeKind::Combo
        | UiNodeKind::Dropdown
        | UiNodeKind::Tabs
        | UiNodeKind::ListBox
        | UiNodeKind::TreeView
        | UiNodeKind::MenuBar
        | UiNodeKind::Accordion
        | UiNodeKind::ContextMenu
        | UiNodeKind::DataGrid => &[Hover, Pressed, Focus, ChoicePreview],
        UiNodeKind::Slider | UiNodeKind::DragValue => &[Hover, Pressed, Focus, NumericPreview],
        UiNodeKind::TextInput => &[Hover, Pressed, Focus, TextEditPreview],
        UiNodeKind::Scrollbar => &[Hover, Pressed, ScrollPreview],
        UiNodeKind::Splitter => &[Pressed, DragPreview],
        UiNodeKind::Panel
        | UiNodeKind::Label
        | UiNodeKind::Image
        | UiNodeKind::RenderSurface
        | UiNodeKind::Canvas
        | UiNodeKind::Tooltip
        | UiNodeKind::Modal
        | UiNodeKind::Dialog
        | UiNodeKind::ProgressBar
        | UiNodeKind::Toast
        | UiNodeKind::Spinner
        | UiNodeKind::Divider
        | UiNodeKind::Popup => return None,
    })
}

fn preview_domains_for(kinds: &[UiInteractionKind]) -> Vec<UiInvalidationDomain> {
    use UiInteractionKind::*;
    let mut domains = Vec::new();
    for kind in kinds {
        match kind {
            Hover | Pressed | Focus | TogglePreview | ChoicePreview | NumericPreview
            | DropResolution => domains.extend([InteractionPresentation, ColorInstances]),
            TextEditPreview => {
                domains.extend([InteractionPresentation, ColorInstances, TextLayout])
            }
            ScrollPreview => domains.extend([InteractionPresentation, Layout, HitTarget]),
            DragPreview => {
                domains.extend([InteractionPresentation, ColorInstances, Layout, HitTarget]);
            }
        }
    }
    sort_dedup_domains(&mut domains);
    domains
}

/// Flow drag/drop declarations are attached after IR compilation, so their
/// interaction consequences are merged into the impact graph here. A drag
/// source gains pointer-driven preview behavior; a drop target records the
/// resolution intent without ever gaining authoritative input writes.
pub(crate) fn apply_drag_drop_interactions(
    impacts: &mut BTreeMap<String, UiInteractionImpact>,
    drag_sources: &[String],
    drop_targets: &[(String, String)],
) {
    let mut extra: BTreeMap<&str, (Vec<UiInteractionKind>, Vec<String>)> = BTreeMap::new();
    for source in drag_sources {
        extra
            .entry(source.as_str())
            .or_default()
            .0
            .push(UiInteractionKind::DragPreview);
    }
    for (target, intent) in drop_targets {
        let entry = extra.entry(target.as_str()).or_default();
        entry.0.push(UiInteractionKind::DropResolution);
        entry.1.push(intent.clone());
    }
    for (node_key, (mut kinds, intents)) in extra {
        kinds.extend([UiInteractionKind::Hover, UiInteractionKind::Pressed]);
        kinds.sort();
        kinds.dedup();
        let existing = impacts.get_mut(node_key);
        match existing {
            Some(existing) => {
                existing.interaction_kinds.extend(kinds.iter().cloned());
                existing.interaction_kinds.sort();
                existing.interaction_kinds.dedup();
                existing.preview_domains = preview_domains_for(&existing.interaction_kinds);
                existing.semantic_intents.extend(intents);
                existing.semantic_intents.sort();
                existing.semantic_intents.dedup();
            }
            None => {
                impacts.insert(
                    node_key.to_owned(),
                    UiInteractionImpact {
                        node_key: node_key.to_owned(),
                        preview_domains: preview_domains_for(&kinds),
                        interaction_kinds: kinds,
                        semantic_intents: {
                            let mut intents: Vec<String> = intents;
                            intents.sort();
                            intents.dedup();
                            intents
                        },
                        controlled_input_keys: Vec::new(),
                    },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile_nui_flow_program;
    use crate::parse_nui_flow;
    use neon_protocol::Revision;
    use neon_ui_schema::{
        UI_PROGRAM_CAPABILITY_NAME, UI_PROGRAM_SCHEMA_VERSION, UiProgramCapability,
        UiProgramCapabilityOwner, UiProgramCapabilityStatus, UiProgramRevision,
    };

    const FLOW: &str = "version 1
surface surface.impact-contract revision 1
budget nodes=16 bindings=16 instances=16 text=8 glyphs=64 events=8 clips=8
input show bool default false
input hp f32 default 0.8
input low_hp bool = $hp < 0.3
input unused bool default true
surface root row w 400 h 300
  branch ready h 100 when $show
    text status value \"ready\" w 100 h 20
  button act event app.act checked $low_hp w 80 h 24
  panel extra visible $low_hp w 80 h 40
";

    fn revision() -> UiProgramRevision {
        UiProgramRevision {
            program_id: "surface.impact-contract".into(),
            revision: Revision(1),
            schema_version: UI_PROGRAM_SCHEMA_VERSION,
            capabilities: vec![UiProgramCapability {
                name: UI_PROGRAM_CAPABILITY_NAME.into(),
                version: 1,
                owner: UiProgramCapabilityOwner::SharedContract,
                status: UiProgramCapabilityStatus::Supported,
            }],
        }
    }

    fn compiled() -> neon_ui_schema::UiProgram {
        let document = parse_nui_flow(FLOW).expect("impact fixture must parse");
        compile_nui_flow_program(&document, revision()).expect("impact fixture must compile")
    }

    #[test]
    fn input_impacts_survive_json_round_trip_byte_identically() {
        let program = compiled();
        let first = serde_json::to_string(&program.dependency_index.input_impacts).unwrap();
        let parsed: BTreeMap<String, UiInputImpact> = serde_json::from_str(&first).unwrap();
        assert_eq!(parsed, program.dependency_index.input_impacts);
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            first,
            "round trip must preserve serialization exactly"
        );
        let interactions =
            serde_json::to_string(&program.dependency_index.interaction_impacts).unwrap();
        let parsed: BTreeMap<String, UiInteractionImpact> =
            serde_json::from_str(&interactions).unwrap();
        assert_eq!(
            serde_json::to_string(&parsed).unwrap(),
            interactions,
            "interaction round trip must preserve serialization exactly"
        );
    }

    #[test]
    fn compilation_is_deterministic_and_covers_every_declared_input() {
        let first = compiled();
        let second = compiled();
        assert_eq!(
            first.dependency_index.input_impacts,
            second.dependency_index.input_impacts
        );
        let document = parse_nui_flow(FLOW).unwrap();
        for slot in &document.input_schema.slots {
            let impact = first
                .dependency_index
                .input_impacts
                .get(&slot.key)
                .unwrap_or_else(|| panic!("missing impact record for {}", slot.key));
            assert_eq!(impact.input_key, slot.key);
        }
        let unused = &first.dependency_index.input_impacts["unused"];
        assert!(unused.binding_ids.is_empty());
        assert!(unused.branch_keys.is_empty());
        assert!(unused.affected_node_keys.is_empty());
        assert!(unused.domains.is_empty());
    }

    #[test]
    fn every_binding_lands_in_its_input_impact_with_the_property_domains() {
        let program = compiled();
        for binding in &program.binding_records {
            let impact = &program.dependency_index.input_impacts[&binding.input_key];
            let recorded = impact
                .binding_impacts
                .iter()
                .find(|entry| entry.binding_id == binding.binding_id)
                .unwrap_or_else(|| panic!("binding {} missing from impact", binding.binding_id));
            assert_eq!(recorded.node_key, binding.node_key);
            assert_eq!(recorded.property, binding.property);
            assert_eq!(recorded.domains, binding.property.invalidation_domains());
            assert!(impact.binding_ids.contains(&binding.binding_id));
            assert!(impact.affected_node_keys.contains(&binding.node_key));
        }
        let visible = &program.dependency_index.input_impacts["low_hp"];
        assert!(
            visible.domains.contains(&UiInvalidationDomain::HitTarget),
            "a visible binding must mark the whole visible domain set"
        );
    }

    #[test]
    fn branch_predicate_input_carries_the_full_branch_range_and_ancestors() {
        let program = compiled();
        let impact = &program.dependency_index.input_impacts["show"];
        assert_eq!(impact.branch_keys, vec!["ready".to_owned()]);
        assert_eq!(
            impact.affected_node_keys,
            vec!["ready".to_owned(), "root".to_owned(), "status".to_owned()],
            "branch subtree plus layout ancestors must all be impacted"
        );
        for domain in BRANCH_DOMAINS {
            assert!(impact.domains.contains(&domain));
        }
    }

    #[test]
    fn impacts_never_reference_unknown_nodes_or_binding_ids() {
        let program = compiled();
        let node_keys: BTreeSet<&str> =
            program.nodes.iter().map(|node| node.key.as_str()).collect();
        let binding_ids: BTreeSet<u32> = program
            .binding_records
            .iter()
            .map(|binding| binding.binding_id)
            .collect();
        for impact in program.dependency_index.input_impacts.values() {
            for key in &impact.affected_node_keys {
                assert!(node_keys.contains(key.as_str()), "unknown node {key}");
            }
            for id in &impact.binding_ids {
                assert!(binding_ids.contains(id), "unknown binding id {id}");
            }
            for binding in &impact.binding_impacts {
                assert!(binding_ids.contains(&binding.binding_id));
                assert!(node_keys.contains(binding.node_key.as_str()));
            }
        }
        for impact in program.dependency_index.interaction_impacts.values() {
            assert!(node_keys.contains(impact.node_key.as_str()));
        }
    }

    #[test]
    fn derived_input_changes_reach_the_derived_slots_consumers() {
        let program = compiled();
        let low_hp = &program.dependency_index.input_impacts["low_hp"];
        let hp = &program.dependency_index.input_impacts["hp"];
        assert!(!low_hp.binding_ids.is_empty());
        assert_eq!(
            hp.binding_ids, low_hp.binding_ids,
            "changing hp must redo exactly what changing low_hp redoes"
        );
        assert_eq!(hp.affected_node_keys, low_hp.affected_node_keys);
        assert!(
            hp.domains.contains(&UiInvalidationDomain::HitTarget),
            "derived consumers inherit the visible-binding hit domain"
        );
    }

    #[test]
    fn interaction_impacts_describe_renderer_preview_scope() {
        let program = compiled();
        let act = &program.dependency_index.interaction_impacts["act"];
        assert_eq!(
            act.interaction_kinds,
            vec![
                UiInteractionKind::Hover,
                UiInteractionKind::Pressed,
                UiInteractionKind::Focus
            ]
        );
        assert_eq!(act.semantic_intents, vec!["app.act".to_owned()]);
        assert_eq!(act.controlled_input_keys, vec!["low_hp".to_owned()]);
        assert_eq!(
            act.preview_domains,
            vec![ColorInstances, InteractionPresentation]
        );
        assert!(
            !program
                .dependency_index
                .interaction_impacts
                .contains_key("status"),
            "a static text node must not appear as an interaction target"
        );
    }
}
