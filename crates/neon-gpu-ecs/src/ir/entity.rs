//! Initial entity populations.

use serde::{Deserialize, Serialize};

/// Defines an initial population of entities sharing the same component set.
/// The runtime materializes `count` entities from each prototype at world
/// creation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntityPrototype {
    /// Components present on every entity of this population.
    pub component_ids: Vec<u32>,
    /// Number of entities to create; must be at least 1.
    pub count: u32,
    /// Optional per-component initial values, in the same order as
    /// `component_ids`. When `None`, or per entry, the registered
    /// `ComponentDef::default_value` is used.
    pub initial_values: Option<Vec<Vec<u8>>>,
}
