//! IR construction: SSA validation and dataflow graph building.

use std::collections::HashMap;

use crate::ast::{Arg, ArgValue, ExportStmt, QualifiedName, Scene, Script, Stmt};
use crate::error::ScriptError;
use crate::registry::{KernelRegistry, WorldRegistry};

/// Node index into `IrScene::nodes`.
pub type NodeId = usize;

#[derive(Debug, Clone, PartialEq)]
pub enum NodeKind {
    /// A scene input bound to a world resource (level 0, never dispatched).
    Input { world: QualifiedName },
    /// A fixed kernel call.
    Kernel { kernel: String },
}

/// One argument of a kernel node: either a data dependency or a baked constant.
#[derive(Debug, Clone, PartialEq)]
pub enum IrArg {
    Value(NodeId),
    Const { key: String, value: ConstValue },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Number(f64),
    Str(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct IrNode {
    pub id: NodeId,
    pub kind: NodeKind,
    pub args: Vec<IrArg>,
    /// Direct data dependencies (positional, `Ident` named args, hoisted calls).
    pub preds: Vec<NodeId>,
    /// The SSA value produced by this node (user name or hoisted `%n`).
    pub result: String,
}

/// A scene after SSA validation, as a dataflow graph.
#[derive(Debug, Clone, PartialEq)]
pub struct IrScene {
    pub name: String,
    /// Input nodes first, in declaration order; `alias -> node id`.
    pub inputs: Vec<(String, NodeId)>,
    pub outputs: Vec<QualifiedName>,
    pub nodes: Vec<IrNode>,
    /// `(target, source node id)` in declaration order.
    pub exports: Vec<(QualifiedName, NodeId)>,
}

/// A scene with its wave plan (topological layering).
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledScene {
    pub ir: IrScene,
    pub waves: Vec<Vec<NodeId>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompiledScript {
    pub scenes: Vec<CompiledScene>,
}

pub fn build(
    script: &Script,
    world: &WorldRegistry,
    kernels: &KernelRegistry,
) -> Result<CompiledScript, ScriptError> {
    let mut writers: HashMap<String, QualifiedName> = HashMap::new();
    for scene in &script.scenes {
        for stmt in &scene.body {
            if let Stmt::Export(ExportStmt { target, .. }) = stmt {
                if writers.contains_key(&target.key()) {
                    return Err(ScriptError::WriterConflict {
                        domain: target.domain.clone(),
                        name: target.name.clone(),
                    });
                }
                writers.insert(target.key(), target.clone());
            }
        }
    }

    let mut scenes = Vec::new();
    for scene in &script.scenes {
        let ir = build_scene(scene, world, kernels)?;
        let waves = crate::plan::layering(&ir);
        scenes.push(CompiledScene { ir, waves });
    }
    Ok(CompiledScript { scenes })
}

fn build_scene(
    scene: &Scene,
    world: &WorldRegistry,
    kernels: &KernelRegistry,
) -> Result<IrScene, ScriptError> {
    let mut builder = NodeBuilder {
        kernels,
        value_defs: HashMap::new(),
        nodes: Vec::new(),
        anon_seq: 0usize,
    };
    let mut inputs = Vec::new();

    for decl in &scene.inputs {
        if world.get(&decl.world).is_none() {
            return Err(ScriptError::UnknownWorld {
                domain: decl.world.domain.clone(),
                name: decl.world.name.clone(),
            });
        }
        if builder.value_defs.contains_key(&decl.alias) {
            return Err(ScriptError::SsaViolation {
                name: decl.alias.clone(),
            });
        }
        let id = builder.nodes.len();
        builder.value_defs.insert(decl.alias.clone(), id);
        inputs.push((decl.alias.clone(), id));
        builder.nodes.push(IrNode {
            id,
            kind: NodeKind::Input {
                world: decl.world.clone(),
            },
            args: Vec::new(),
            preds: Vec::new(),
            result: decl.alias.clone(),
        });
    }

    for q in &scene.outputs {
        let spec = world.get(q).ok_or_else(|| ScriptError::UnknownWorld {
            domain: q.domain.clone(),
            name: q.name.clone(),
        })?;
        if !spec.writable {
            return Err(ScriptError::ReadOnlyOutput {
                domain: q.domain.clone(),
                name: q.name.clone(),
            });
        }
    }

    let mut exports = Vec::new();

    for stmt in &scene.body {
        match stmt {
            Stmt::Let(stmt) => {
                if builder.value_defs.contains_key(&stmt.name) {
                    return Err(ScriptError::SsaViolation {
                        name: stmt.name.clone(),
                    });
                }
                builder.build_call(&stmt.kernel, &stmt.args, stmt.name.clone())?;
            }
            Stmt::Export(stmt) => {
                let source = *builder.value_defs.get(&stmt.source).ok_or_else(|| {
                    ScriptError::UndefinedValue {
                        name: stmt.source.clone(),
                    }
                })?;
                if !scene.outputs.contains(&stmt.target) {
                    return Err(ScriptError::UndeclaredOutput {
                        domain: stmt.target.domain.clone(),
                        name: stmt.target.name.clone(),
                    });
                }
                exports.push((stmt.target.clone(), source));
            }
        }
    }

    Ok(IrScene {
        name: scene.name.clone(),
        inputs,
        outputs: scene.outputs.clone(),
        nodes: builder.nodes,
        exports,
    })
}

struct NodeBuilder<'a> {
    kernels: &'a KernelRegistry,
    value_defs: HashMap<String, NodeId>,
    nodes: Vec<IrNode>,
    anon_seq: usize,
}

impl NodeBuilder<'_> {
    /// Builds a kernel node, recursively hoisting nested [`Arg::Call`]s into
    /// anonymous SSA nodes. The node's result is registered under `result`.
    fn build_call(
        &mut self,
        kernel: &str,
        args: &[Arg],
        result: String,
    ) -> Result<NodeId, ScriptError> {
        let spec = self
            .kernels
            .get(kernel)
            .ok_or_else(|| ScriptError::UnknownKernel {
                name: kernel.to_string(),
            })?;

        let mut ir_args = Vec::new();
        let mut preds = Vec::new();
        let mut positional = 0usize;

        for arg in args {
            match arg {
                Arg::Pos(ArgValue::Ident(name)) => {
                    let dep = self.lookup(name)?;
                    preds.push(dep);
                    ir_args.push(IrArg::Value(dep));
                    positional += 1;
                }
                Arg::Pos(ArgValue::Number(n)) => {
                    ir_args.push(IrArg::Const {
                        key: format!("#{positional}"),
                        value: ConstValue::Number(*n),
                    });
                    positional += 1;
                }
                Arg::Pos(ArgValue::Str(s)) => {
                    ir_args.push(IrArg::Const {
                        key: format!("#{positional}"),
                        value: ConstValue::Str(s.clone()),
                    });
                    positional += 1;
                }
                Arg::Call(call) => {
                    let anon = format!("%{}", self.anon_seq);
                    self.anon_seq += 1;
                    let dep = self.build_call(&call.kernel, &call.args, anon)?;
                    preds.push(dep);
                    ir_args.push(IrArg::Value(dep));
                    positional += 1;
                }
                Arg::Named { key, value, .. } => {
                    if !spec.params.iter().any(|p| p == key) {
                        return Err(ScriptError::UnknownParam {
                            name: kernel.to_string(),
                            param: key.clone(),
                        });
                    }
                    match value {
                        ArgValue::Number(n) => {
                            ir_args.push(IrArg::Const {
                                key: key.clone(),
                                value: ConstValue::Number(*n),
                            });
                        }
                        ArgValue::Str(s) => {
                            ir_args.push(IrArg::Const {
                                key: key.clone(),
                                value: ConstValue::Str(s.clone()),
                            });
                        }
                        ArgValue::Ident(name) => {
                            let dep = self.lookup(name)?;
                            preds.push(dep);
                            ir_args.push(IrArg::Value(dep));
                        }
                    }
                }
            }
        }

        if positional != spec.value_args {
            return Err(ScriptError::KernelArgCount {
                name: kernel.to_string(),
                expected: spec.value_args,
                actual: positional,
            });
        }

        let id = self.value_defs.len();
        self.value_defs.insert(result.clone(), id);
        self.nodes.push(IrNode {
            id,
            kind: NodeKind::Kernel {
                kernel: kernel.to_string(),
            },
            args: ir_args,
            preds,
            result,
        });
        Ok(id)
    }

    fn lookup(&self, name: &str) -> Result<NodeId, ScriptError> {
        self.value_defs
            .get(name)
            .copied()
            .ok_or_else(|| ScriptError::UndefinedValue {
                name: name.to_string(),
            })
    }
}
