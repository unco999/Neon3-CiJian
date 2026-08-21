//! Codelet contract: a fixed kernel's WGSL generator.
//!
//! A codelet knows how to emit WGSL for one logical kernel given its baked
//! constant arguments and the element count `n`. Positional constants are
//! keyed by their index (`#0`, `#1`, ...); named constants by their name.
//! Value inputs (storage buffers) are bound in dependency order and the
//! output is the last binding.

use neon_gpu_script::{ConstValue, IrArg};

/// Element type of a field (affects WGSL declarations and buffer sizing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldTy {
    F32,
    U32,
}

impl FieldTy {
    pub fn bytes(self) -> u32 {
        match self {
            FieldTy::F32 | FieldTy::U32 => 4,
        }
    }
}

/// A resolved constant argument passed to [`Codelet::wgsl`].
#[derive(Debug, Clone, PartialEq)]
pub struct ConstArg {
    pub key: String,
    pub value: ConstValue,
}

impl ConstArg {
    pub fn as_f32(&self) -> Option<f32> {
        match self.value {
            ConstValue::Number(n) => Some(n as f32),
            ConstValue::Str(_) => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.value {
            ConstValue::Str(s) => Some(s),
            ConstValue::Number(_) => None,
        }
    }
}

/// A fixed-kernel WGSL generator plus its binding contract.
pub trait Codelet: Send + Sync {
    /// How many storage inputs (value dependencies) this kernel consumes.
    fn input_count(&self) -> usize;

    /// Which constant keys are allowed (for compile-time validation).
    fn allowed_consts(&self) -> Vec<String>;

    /// Whether this kernel accepts the given value count and constant keys.
    /// Defaults to `value_count == input_count()` with all keys allowed.
    fn accepts(&self, value_count: usize, const_keys: &[String]) -> bool {
        value_count == self.input_count()
            && const_keys.iter().all(|k| self.allowed_consts().contains(k))
    }

    /// Emit the WGSL module. `consts` contains exactly the allowed constants
    /// that were present in the script (positional constants keyed `#i`).
    /// `n` is the element count of every bound field.
    fn wgsl(&self, consts: &[ConstArg], n: u32, value_types: &[FieldTy]) -> String;
}

/// Helper to filter the value/const split of a node's IR args.
pub fn split_args(args: &[IrArg]) -> (Vec<usize>, Vec<ConstArg>) {
    let mut values = Vec::new();
    let mut consts = Vec::new();
    for arg in args {
        match arg {
            IrArg::Value(id) => values.push(*id),
            IrArg::Const { key, value } => consts.push(ConstArg {
                key: key.clone(),
                value: value.clone(),
            }),
        }
    }
    (values, consts)
}
