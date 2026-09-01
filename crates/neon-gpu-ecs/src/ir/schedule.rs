//! Macro execution schedule: ordered stages of systems.

use serde::{Deserialize, Serialize};

/// The execution schedule. Stages run sequentially with an implicit barrier
/// between them; systems inside one stage are dispatched back to back in a
/// single compute pass. The validator rejects stages whose systems write the
/// same component, so intra-stage ordering is safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduleDef {
    pub stages: Vec<Stage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stage {
    /// Must equal the stage's index inside `ScheduleDef::stages`.
    pub id: u32,
    pub name: String,
    /// Systems dispatched in this stage, in order.
    pub system_ids: Vec<u32>,
}
