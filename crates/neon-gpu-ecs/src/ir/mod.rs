//! The language-agnostic ECS intermediate representation.
//!
//! `EcsIr` is the static blueprint of a world: registered components and
//! resources, initial entity populations, queries, TAC system bodies and the
//! macro schedule. It derives `serde::{Serialize, Deserialize}` so frontends
//! in other languages (C#/Python/Lua) can produce it.

pub mod component;
pub mod entity;
pub mod query;
pub mod resource;
pub mod schedule;
pub mod system;
pub mod types;

pub use component::ComponentDef;
pub use entity::EntityPrototype;
pub use query::{AccessType, ComponentAccess, QueryDef, QueryFilter};
pub use resource::{ResourceDef, ResourceRef, RESERVED_RENDER_BINDING};
pub use schedule::{ScheduleDef, Stage};
pub use system::{
    AtomicOpCode, BinaryOpCode, BuiltinFunc, CompareOp, Instr, RenderField, SystemDef,
    UnaryOpCode,
};
pub use types::ComponentType;

use crate::EcsError;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Top-level IR package describing one world.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EcsIr {
    /// Schema version for compatibility checks.
    pub version: u32,
    /// Registered component types; index == `ComponentDef::id`.
    pub components: Vec<ComponentDef>,
    /// Registered fixed resources; index == `ResourceDef::id`.
    pub resources: Vec<ResourceDef>,
    /// Initial entity populations.
    pub initial_entities: Vec<EntityPrototype>,
    /// Queries; index == `QueryDef::id`.
    pub queries: Vec<QueryDef>,
    /// Systems; index == `SystemDef::id`.
    pub systems: Vec<SystemDef>,
    /// Stage schedule.
    pub schedule: ScheduleDef,
}

impl EcsIr {
    /// Validate the whole IR. Returns every problem found, or `Ok(())`.
    pub fn validate(&self) -> Result<(), EcsError> {
        let mut problems: Vec<String> = Vec::new();

        if self.version != 1 {
            problems.push(format!("unsupported ir version {} (expected 1)", self.version));
        }

        // --- components ---
        let mut comp_names = HashSet::new();
        for (index, comp) in self.components.iter().enumerate() {
            let id = comp.id as usize;
            if id != index {
                problems.push(format!(
                    "component '{0}' id {1} does not match its index {index}",
                    comp.name, comp.id
                ));
            }
            if comp.name.is_empty() {
                problems.push(format!("component id {index} has an empty name"));
            } else if !is_wgsl_ident(&comp.name) {
                problems.push(format!(
                    "component '{}' name is not a valid WGSL identifier",
                    comp.name
                ));
            }
            if !comp_names.insert(comp.name.clone()) {
                problems.push(format!("duplicate component name '{}'", comp.name));
            }
            if comp.default_value.len() != comp.ty.byte_size() {
                problems.push(format!(
                    "component '{}' default_value is {} bytes, expected {}",
                    comp.name,
                    comp.default_value.len(),
                    comp.ty.byte_size()
                ));
            }
        }
        let n_comp = self.components.len();

        // --- resources ---
        let mut slots = HashSet::new();
        let mut res_names = HashSet::new();
        for (index, res) in self.resources.iter().enumerate() {
            let id = res.id as usize;
            if id != index {
                problems.push(format!(
                    "resource '{}' id {} does not match its index {}",
                    res.name, res.id, index
                ));
            }
            if res.name.is_empty() || !is_wgsl_ident(&res.name) {
                problems.push(format!("resource id {index} has an invalid name '{}'", res.name));
            }
            if !res_names.insert(res.name.clone()) {
                problems.push(format!("duplicate resource name '{}'", res.name));
            }
            if res.binding_slot >= RESERVED_RENDER_BINDING {
                problems.push(format!(
                    "resource '{}' binding_slot {} collides with reserved slots >= {}",
                    res.name, res.binding_slot, RESERVED_RENDER_BINDING
                ));
            }
            if !slots.insert(res.binding_slot) {
                problems.push(format!(
                    "resource '{}' duplicates binding_slot {}",
                    res.name, res.binding_slot
                ));
            }
            if res.default_value.len() != res.ty.byte_size() {
                problems.push(format!(
                    "resource '{}' default_value is {} bytes, expected {}",
                    res.name,
                    res.default_value.len(),
                    res.ty.byte_size()
                ));
            }
            if !res.ty.is_uniform_safe() {
                problems.push(format!(
                    "resource '{}' type {:?} cannot back a uniform binding",
                    res.name, res.ty
                ));
            }
        }
        let n_res = self.resources.len();

        // --- queries ---
        let mut render_queries = 0u32;
        for (index, query) in self.queries.iter().enumerate() {
            if query.id as usize != index {
                problems.push(format!("query id {} does not match its index {index}", query.id));
            }
            if query.with.is_empty() {
                problems.push(format!("query {index} requires at least one component"));
            }
            if query.filters.contains(&QueryFilter::RenderData) {
                render_queries += 1;
            }
            let mut seen = HashSet::new();
            for access in &query.with {
                if access.component_id as usize >= n_comp {
                    problems.push(format!(
                        "query {index} references unknown component {}",
                        access.component_id
                    ));
                }
                if !seen.insert(access.component_id) {
                    problems.push(format!(
                        "query {index} lists component {} twice",
                        access.component_id
                    ));
                }
            }
            for cid in &query.without {
                if *cid as usize >= n_comp {
                    problems.push(format!("query {index} without-clause references unknown component {cid}"));
                } else if seen.contains(cid) {
                    problems.push(format!(
                        "query {index} lists component {cid} in both with and without"
                    ));
                }
            }
            for filter in &query.filters {
                let cid = match filter {
                    QueryFilter::Changed(c) | QueryFilter::Added(c) => Some(*c),
                    QueryFilter::RenderData => None,
                };
                match cid {
                    Some(cid) if (cid as usize) >= n_comp => {
                        problems.push(format!(
                            "query {index} filter references unknown component {cid}"
                        ));
                    }
                    _ => {}
                }
            }
        }
        let n_query = self.queries.len();
        if render_queries > 1 {
            problems.push(format!(
                "{render_queries} queries carry the RenderData filter; at most one is allowed"
            ));
        }

        // --- systems ---
        let mut sys_names = HashSet::new();
        for (index, system) in self.systems.iter().enumerate() {
            if system.id as usize != index {
                problems.push(format!(
                    "system '{}' id {} does not match its index {index}",
                    system.name, system.id
                ));
            }
            if system.name.is_empty() || !is_wgsl_ident(&system.name) {
                problems.push(format!("system id {index} has an invalid name '{}'", system.name));
            }
            if !sys_names.insert(system.name.clone()) {
                problems.push(format!("duplicate system name '{}'", system.name));
            }
            if system.query_id as usize >= n_query {
                problems.push(format!("system '{}' references unknown query {}", system.name, system.query_id));
            }
            for r in &system.resource_refs {
                if r.resource_id as usize >= n_res {
                    problems.push(format!(
                        "system '{}' references unknown resource {}",
                        system.name, r.resource_id
                    ));
                }
                if r.access_type.is_write() {
                    problems.push(format!(
                        "system '{}' writes resource {}; resources are read-only in v1",
                        system.name, r.resource_id
                    ));
                }
            }
            let query = self.queries.get(system.query_id as usize);
            let query_with = query.map(|q| q.with.as_slice()).unwrap_or(&[]);
            let render_query = query
                .map(|q| q.filters.contains(&QueryFilter::RenderData))
                .unwrap_or(false);
            problems.extend(validate_system_body(
                system,
                &self.components,
                n_comp,
                n_res,
                query_with,
                render_query,
            ));
        }
        let n_sys = self.systems.len();

        // --- schedule ---
        let mut covered = HashSet::new();
        for (index, stage) in self.schedule.stages.iter().enumerate() {
            if stage.id as usize != index {
                problems.push(format!("stage id {} does not match its index {index}", stage.id));
            }
            for sid in &stage.system_ids {
                if *sid as usize >= n_sys {
                    problems.push(format!("stage {index} references unknown system {sid}"));
                } else if !covered.insert(*sid) {
                    problems.push(format!("system {sid} appears in more than one stage"));
                }
            }
            // Intra-stage write conflict detection (mirrors generator::validation).
            let mut writes: HashMap<u32, u32> = HashMap::new();
            for sid in &stage.system_ids {
                if let Some(system) = self.systems.get(*sid as usize) {
                    for cid in system.written_components() {
                        if let Some(other) = writes.get(&cid) {
                            problems.push(format!(
                                "stage {index}: systems {other} and {sid} both write component {cid}"
                            ));
                        } else {
                            writes.insert(cid, *sid);
                        }
                    }
                }
            }
        }
        for sid in 0..n_sys {
            if !covered.contains(&(sid as u32)) {
                problems.push(format!("system {sid} is not referenced by any stage"));
            }
        }

        // --- prototypes ---
        for (index, proto) in self.initial_entities.iter().enumerate() {
            if proto.count == 0 {
                problems.push(format!("prototype {index} count must be >= 1"));
            }
            let mut seen = HashSet::new();
            for cid in &proto.component_ids {
                if *cid as usize >= n_comp {
                    problems.push(format!("prototype {index} references unknown component {cid}"));
                }
                if !seen.insert(*cid) {
                    problems.push(format!("prototype {index} lists component {cid} twice"));
                }
            }
            if let Some(values) = &proto.initial_values {
                if values.len() != proto.component_ids.len() {
                    problems.push(format!(
                        "prototype {index} initial_values length {} != component_ids length {}",
                        values.len(),
                        proto.component_ids.len()
                    ));
                } else {
                    for (cid, bytes) in proto.component_ids.iter().zip(values) {
                        let expected = match self.components.get(*cid as usize) {
                            Some(comp) => comp.ty.byte_size(),
                            None => continue,
                        };
                        if bytes.len() != expected {
                            problems.push(format!(
                                "prototype {index} value for component {cid} is {} bytes, expected {}",
                                bytes.len(),
                                expected
                            ));
                        }
                    }
                }
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(EcsError::IrInvalid(problems.join("; ")))
        }
    }
}

/// Validate one system's TAC body against component/resource registries.
/// `query_with` is the resolved `with` list of the system's query;
/// `render_query` is true when that query carries the RenderData filter.
fn validate_system_body(
    system: &SystemDef,
    components: &[ComponentDef],
    n_comp: usize,
    n_res: usize,
    query_with: &[ComponentAccess],
    render_query: bool,
) -> Vec<String> {
    let mut problems = Vec::new();
    let tag = || format!("system '{}'", system.name);
    let n_locals = system.local_var_count;
    let n_instr = system.instructions.len();

    // Resource references used by LoadResource must be declared.
    let declared_resources: HashSet<u32> = system
        .resource_refs
        .iter()
        .map(|r| r.resource_id)
        .collect();

    // Components touched must be present in the query's `with` list.
    let query_components: HashSet<u32> =
        query_with.iter().map(|a| a.component_id).collect();

    for (pc, instr) in system.instructions.iter().enumerate() {
        match instr {
            Instr::Load { dest, component_id, .. } => {
                check_local(&mut problems, &tag(), *dest, n_locals, pc);
                check_component(&mut problems, &tag(), *component_id, n_comp, pc);
                if (*component_id as usize) < n_comp && !query_components.contains(component_id) {
                    problems.push(format!(
                        "{} [{pc}]: Load of component {component_id} not covered by query",
                        tag()
                    ));
                }
            }
            Instr::Const { dest, ty, bytes } => {
                check_local(&mut problems, &tag(), *dest, n_locals, pc);
                if matches!(ty, ComponentType::Mat4F) {
                    problems.push(format!("{} [{pc}]: Const of mat4x4f is not supported", tag()));
                }
                if bytes.len() != ty.byte_size() {
                    problems.push(format!(
                        "{} [{pc}]: Const literal is {} bytes, expected {}",
                        tag(),
                        bytes.len(),
                        ty.byte_size()
                    ));
                }
            }
            Instr::LoadEntityId { dest } => {
                check_local(&mut problems, &tag(), *dest, n_locals, pc);
            }
            Instr::StoreRender { src, .. } => {
                check_local(&mut problems, &tag(), *src, n_locals, pc);
                if !render_query {
                    problems.push(format!(
                        "{} [{pc}]: StoreRender requires a RenderData query",
                        tag()
                    ));
                }
            }
            Instr::Store { src, component_id } => {
                check_local(&mut problems, &tag(), *src, n_locals, pc);
                check_component(&mut problems, &tag(), *component_id, n_comp, pc);
                if (*component_id as usize) < n_comp && !query_components.contains(component_id) {
                    problems.push(format!(
                        "{} [{pc}]: Store to component {component_id} not covered by query",
                        tag()
                    ));
                }
            }
            Instr::LoadResource { dest, resource_id } => {
                check_local(&mut problems, &tag(), *dest, n_locals, pc);
                if *resource_id as usize >= n_res {
                    problems.push(format!("{} [{pc}]: unknown resource {resource_id}", tag()));
                } else if !declared_resources.contains(resource_id) {
                    problems.push(format!(
                        "{} [{pc}]: resource {resource_id} not declared in resource_refs",
                        tag()
                    ));
                }
            }
            Instr::BinaryOp { dest, lhs, rhs, .. } => {
                check_local(&mut problems, &tag(), *dest, n_locals, pc);
                check_local(&mut problems, &tag(), *lhs, n_locals, pc);
                check_local(&mut problems, &tag(), *rhs, n_locals, pc);
            }
            Instr::UnaryOp { dest, src, .. } => {
                check_local(&mut problems, &tag(), *dest, n_locals, pc);
                check_local(&mut problems, &tag(), *src, n_locals, pc);
            }
            Instr::Compare { dest, lhs, rhs, .. } => {
                check_local(&mut problems, &tag(), *dest, n_locals, pc);
                check_local(&mut problems, &tag(), *lhs, n_locals, pc);
                check_local(&mut problems, &tag(), *rhs, n_locals, pc);
            }
            Instr::If { cond, true_block, false_block } => {
                check_local(&mut problems, &tag(), *cond, n_locals, pc);
                check_target(&mut problems, &tag(), *true_block, n_instr, pc, "If.true_block");
                check_target(&mut problems, &tag(), *false_block, n_instr, pc, "If.false_block");
            }
            Instr::Jump { target } => {
                check_target(&mut problems, &tag(), *target, n_instr, pc, "Jump.target");
            }
            Instr::Return => {}
            Instr::CallBuiltin { dest, func, args } => {
                check_local(&mut problems, &tag(), *dest, n_locals, pc);
                for arg in args {
                    check_local(&mut problems, &tag(), *arg, n_locals, pc);
                }
                let expected = builtin_arity(*func);
                if args.len() != expected {
                    problems.push(format!(
                        "{} [{pc}]: builtin {:?} expects {expected} args, got {}",
                        tag(),
                        func,
                        args.len()
                    ));
                }
            }
            Instr::AtomicOp { component_id, value, .. } => {
                check_local(&mut problems, &tag(), *value, n_locals, pc);
                check_component(&mut problems, &tag(), *component_id, n_comp, pc);
                let atomic_on_bool = components
                    .get(*component_id as usize)
                    .is_some_and(|comp| comp.ty == ComponentType::Bool);
                if atomic_on_bool {
                    problems.push(format!(
                        "{} [{pc}]: atomic on bool component {component_id} is not supported",
                        tag()
                    ));
                }
            }
        }
    }
    problems
}

fn check_local(problems: &mut Vec<String>, tag: &str, slot: u32, n_locals: u32, pc: usize) {
    if slot >= n_locals {
        problems.push(format!("{tag} [{pc}]: local v{slot} out of range (local_var_count = {n_locals})"));
    }
}

fn check_component(problems: &mut Vec<String>, tag: &str, cid: u32, n_comp: usize, pc: usize) {
    if cid as usize >= n_comp {
        problems.push(format!("{tag} [{pc}]: unknown component {cid}"));
    }
}

fn check_target(problems: &mut Vec<String>, tag: &str, target: u32, n_instr: usize, pc: usize, what: &str) {
    if target as usize >= n_instr {
        problems.push(format!("{tag} [{pc}]: {what} {target} out of range (len {n_instr})"));
    }
}

/// Expected argument count per builtin.
fn builtin_arity(func: BuiltinFunc) -> usize {
    match func {
        BuiltinFunc::Sin | BuiltinFunc::Cos | BuiltinFunc::Normalize | BuiltinFunc::Length => 1,
        BuiltinFunc::Dot | BuiltinFunc::Cross | BuiltinFunc::AddComponent => 2,
        BuiltinFunc::SpawnEntity => 1,
        BuiltinFunc::DeleteEntity => 1,
        BuiltinFunc::RemoveComponent => 2,
    }
}

/// True when the name is a safe WGSL identifier: [A-Za-z_][A-Za-z0-9_]*.
fn is_wgsl_ident(name: &str) -> bool {
    let mut chars = name.chars();
    let first = match chars.next() {
        Some(c) => c,
        None => return false,
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}
