//! World resource table and fixed-kernel registry.

use std::collections::HashMap;

use crate::ast::QualifiedName;

/// A registered world resource: the authority for `domain.name` resolution.
///
/// The registry is maintained by the runtime / domain layer, not by scripts.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceSpec {
    pub domain: String,
    pub name: String,
    /// Resource kind description, e.g. `field<f32, [64,64]>`.
    pub kind: String,
    /// Whether a scene may declare this resource as an output (single writer).
    pub writable: bool,
}

impl QualifiedName {
    pub fn key(&self) -> String {
        format!("{}.{}", self.domain, self.name)
    }
}

#[derive(Debug, Default)]
pub struct WorldRegistry {
    resources: HashMap<String, ResourceSpec>,
}

impl WorldRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, domain: &str, name: &str, kind: &str, writable: bool) {
        self.resources.insert(
            format!("{domain}.{name}"),
            ResourceSpec {
                domain: domain.to_string(),
                name: name.to_string(),
                kind: kind.to_string(),
                writable,
            },
        );
    }

    pub fn get(&self, q: &QualifiedName) -> Option<&ResourceSpec> {
        self.resources.get(&q.key())
    }
}

/// A registered fixed kernel (Codelet): positional value args plus named
/// constant/value params.
#[derive(Debug, Clone, PartialEq)]
pub struct KernelSpec {
    pub id: String,
    /// Number of positional arguments (data references).
    pub value_args: usize,
    /// Allowed named parameter names.
    pub params: Vec<String>,
}

#[derive(Debug, Default)]
pub struct KernelRegistry {
    kernels: HashMap<String, KernelSpec>,
}

impl KernelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, id: &str, value_args: usize, params: &[&str]) {
        self.kernels.insert(
            id.to_string(),
            KernelSpec {
                id: id.to_string(),
                value_args,
                params: params.iter().map(|s| s.to_string()).collect(),
            },
        );
    }

    pub fn get(&self, id: &str) -> Option<&KernelSpec> {
        self.kernels.get(id)
    }
}