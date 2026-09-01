//! Generator-level validation: schedule conflicts and binding limits.

use crate::ir::EcsIr;
use crate::EcsError;
use std::collections::HashMap;

/// Group 0 storage bindings needed for this world: 8 fixed slots plus three
/// (data / version / baseline) per component.
pub fn required_group0_bindings(ir: &EcsIr) -> u32 {
    super::bind_layout::group0_storage_bindings(ir.components.len() as u32)
}

/// Reject stages whose systems write the same component. Dispatches inside
/// one compute pass are barrier-separated, but intra-stage write overlap
/// would make frame results order-dependent; the contract forbids it.
pub fn check_schedule_conflicts(ir: &EcsIr) -> Result<(), EcsError> {
    for (index, stage) in ir.schedule.stages.iter().enumerate() {
        let mut writes: HashMap<u32, u32> = HashMap::new();
        for sid in &stage.system_ids {
            let Some(system) = ir.systems.get(*sid as usize) else {
                continue;
            };
            for cid in system.written_components() {
                if let Some(other) = writes.get(&cid) {
                    return Err(EcsError::ScheduleConflict(format!(
                        "stage {index} ('{}'): systems {other} and {sid} both write component {cid}",
                        stage.name
                    )));
                }
                writes.insert(cid, *sid);
            }
        }
    }
    Ok(())
}

/// Check the world fits into the device's per-stage storage buffer limit.
pub fn check_limits(ir: &EcsIr, max_storage_buffers: u32) -> Result<(), EcsError> {
    let need = required_group0_bindings(ir);
    if need > max_storage_buffers {
        return Err(EcsError::Limits(format!(
            "world needs {need} group 0 storage bindings but the device allows {max_storage_buffers}; raise max_storage_buffers_per_shader_stage"
        )));
    }
    Ok(())
}
