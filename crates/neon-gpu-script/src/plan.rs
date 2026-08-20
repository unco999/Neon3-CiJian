//! Topological layering: `level(v) = max(level(preds)) + 1`.

use crate::ir::{IrScene, NodeId, NodeKind};

/// Computes the wave plan for a scene.
///
/// Input nodes sit at level 0 and are excluded from the returned waves.
/// Nodes in the same wave have no dependencies between them and may be
/// fused or issued in parallel; the wave count is the critical path length
/// in dispatches.
pub fn layering(scene: &IrScene) -> Vec<Vec<NodeId>> {
    let mut levels = vec![0u32; scene.nodes.len()];

    for (id, node) in scene.nodes.iter().enumerate() {
        if matches!(node.kind, NodeKind::Kernel { .. }) {
            let m = node.preds.iter().map(|&p| levels[p]).max().unwrap_or(0);
            levels[id] = m + 1;
        }
    }

    let mut by_level: std::collections::BTreeMap<u32, Vec<NodeId>> = std::collections::BTreeMap::new();
    for (id, node) in scene.nodes.iter().enumerate() {
        if matches!(node.kind, NodeKind::Kernel { .. }) {
            by_level.entry(levels[id]).or_default().push(id);
        }
    }
    by_level.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::QualifiedName;
    use crate::ir::{IrNode, NodeKind};

    fn input_node(id: usize, alias: &str) -> IrNode {
        IrNode {
            id,
            kind: NodeKind::Input { world: QualifiedName { domain: "t".into(), name: alias.into() } },
            args: Vec::new(),
            preds: Vec::new(),
            result: alias.into(),
        }
    }

    #[test]
    fn levels_follow_max_pred_level() {
        // a(0) b(0) -> c(1) -> d(2); e depends on c and b -> level 2
        let scene = IrScene {
            name: "t".into(),
            inputs: vec![("a".into(), 0), ("b".into(), 1)],
            outputs: Vec::new(),
            nodes: vec![
                input_node(0, "a"),
                input_node(1, "b"),
                IrNode {
                    id: 2,
                    kind: NodeKind::Kernel { kernel: "k1".into() },
                    args: Vec::new(),
                    preds: vec![0, 1],
                    result: "c".into(),
                },
                IrNode {
                    id: 3,
                    kind: NodeKind::Kernel { kernel: "k2".into() },
                    args: Vec::new(),
                    preds: vec![2],
                    result: "d".into(),
                },
                IrNode {
                    id: 4,
                    kind: NodeKind::Kernel { kernel: "k3".into() },
                    args: Vec::new(),
                    preds: vec![2, 1],
                    result: "e".into(),
                },
            ],
            exports: Vec::new(),
        };

        let waves = layering(&scene);
        assert_eq!(waves, vec![vec![2], vec![3, 4]]);
    }
}