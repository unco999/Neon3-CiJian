//! The authoritative, CPU-side allocator for a `neon-gpu` data pool.
//!
//! This is deliberately **pure logic**: it knows nothing about wgpu, buffers
//! or mapping. It owns the layout, the free list, the per-slot generations and
//! the deferred-free queue. The GPU half of a pool (in `crate::gpu`) consumes
//! this allocator and maps it onto a storage buffer.
//!
//! ## Deletion model
//!
//! A freed slot is **not** reusable immediately: the GPU may still be reading
//! it from an in-flight submission. Instead a free is *deferred* until a
//! caller-declared frame count has passed (`advance_frame`). Only then is the
//! slot pushed back onto the free list, with its generation incremented so any
//! stale [`Handle`] no longer resolves. This gives us both safety (no
//! overwriting memory the GPU can still see) and staleness detection (a reused
//! slot never looks like the same element to an old handle).

use crate::handle::Handle;
use crate::layout::StructLayout;

/// Default number of CPU frames a freed slot stays retired before reuse.
pub const DEFAULT_DEFERRED_FREE_FRAMES: u64 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SlotState {
    Free,
    Live,
    Retiring { due_frame: u64 },
}

/// A pool of fixed-size elements.
#[derive(Debug)]
pub struct DataPool {
    layout: StructLayout,
    capacity: u32,
    /// Free slot indices, LIFO.
    free: Vec<u32>,
    /// Next never-yet-used slot index.
    next: u32,
    generations: Vec<u32>,
    states: Vec<SlotState>,
    live: u32,
    /// Monotonic mutation counter. Bumped on alloc/free/write.
    version: u64,
    deferred_free: Vec<(u32, u64)>,
    deferred_frames: u64,
    /// Current CPU frame, set by `advance_frame`.
    frame: u64,
}

/// Errors from pool operations.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PoolError {
    #[error("pool is full (capacity {capacity})")]
    PoolFull { capacity: u32 },
    #[error("handle slot {slot} is out of bounds (capacity {capacity})")]
    OutOfBounds { slot: u32, capacity: u32 },
    #[error("handle generation {handle_gen} does not match slot generation {slot_gen}")]
    StaleGeneration { handle_gen: u32, slot_gen: u32 },
    #[error("element is not live")]
    NotLive,
}

impl DataPool {
    /// Create a pool for `layout` elements with the given fixed capacity.
    pub fn new(layout: StructLayout, capacity: u32) -> Self {
        Self::with_deferred_frames(layout, capacity, DEFAULT_DEFERRED_FREE_FRAMES)
    }

    pub fn with_deferred_frames(layout: StructLayout, capacity: u32, deferred_frames: u64) -> Self {
        Self {
            layout,
            capacity,
            free: Vec::new(),
            next: 0,
            generations: vec![0; capacity as usize],
            states: vec![SlotState::Free; capacity as usize],
            live: 0,
            version: 0,
            deferred_free: Vec::new(),
            deferred_frames,
            frame: 0,
        }
    }

    /// Element layout this pool was created with.
    pub fn layout(&self) -> &StructLayout {
        &self.layout
    }

    pub fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Number of live elements.
    pub fn live_count(&self) -> u32 {
        self.live
    }

    pub fn free_count(&self) -> u32 {
        self.capacity - self.live - self.deferred_free.len() as u32
    }

    /// Monotonic mutation counter.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Allocate a slot. Returns a stable handle; the slot is zero-initialized
    /// by the caller (the GPU half does this on the mapped buffer).
    pub fn alloc(&mut self) -> Result<Handle, PoolError> {
        let slot = if let Some(slot) = self.free.pop() {
            slot
        } else if self.next < self.capacity {
            let slot = self.next;
            self.next += 1;
            slot
        } else {
            return Err(PoolError::PoolFull {
                capacity: self.capacity,
            });
        };
        let generation = self.generations[slot as usize];
        self.states[slot as usize] = SlotState::Live;
        self.live += 1;
        self.version += 1;
        Ok(Handle::new(slot, generation))
    }

    /// Check whether `handle` is currently live.
    pub fn is_live(&self, handle: Handle) -> bool {
        self.resolve(handle).is_ok()
    }

    /// Translate a live handle to a byte offset inside the pool storage.
    ///
    /// This is the *only* place a handle becomes a GPU address, which is why a
    /// stale handle can never silently turn into a live-looking pointer.
    pub fn resolve(&self, handle: Handle) -> Result<crate::handle::GpuPtr, PoolError> {
        let slot = handle.slot;
        let Some(&state) = self.states.get(slot as usize) else {
            return Err(PoolError::OutOfBounds {
                slot,
                capacity: self.capacity,
            });
        };
        if state != SlotState::Live {
            return Err(PoolError::NotLive);
        }
        let slot_gen = self.generations[slot as usize];
        if slot_gen != handle.generation {
            return Err(PoolError::StaleGeneration {
                handle_gen: handle.generation,
                slot_gen,
            });
        }
        let offset = slot * self.layout.array_stride;
        Ok(crate::handle::GpuPtr::new(offset, self.layout.size))
    }

    /// Free a live element. The slot stays retired until the deferred-free
    /// window passes (see [`DataPool::advance_frame`]).
    pub fn free(&mut self, handle: Handle) -> Result<(), PoolError> {
        self.resolve(handle)?; // validates liveness + generation
        let slot = handle.slot as usize;
        self.states[slot] = SlotState::Retiring {
            due_frame: self.frame + self.deferred_frames,
        };
        self.live -= 1;
        self.version += 1;
        self.deferred_free
            .push((handle.slot, self.frame + self.deferred_frames));
        Ok(())
    }

    /// Declare that a new CPU frame has started. Slots whose retirement
    /// window has elapsed are returned to the free list with a bumped
    /// generation.
    ///
    /// Returns the slots that were reclaimed this frame, so the caller can
    /// zero them out (tombstone) before they are reused.
    ///
    /// The caller is responsible for ensuring the GPU can no longer read a
    /// retired slot before the frame that would reuse it (e.g. by waiting on
    /// the submission that referenced it).
    pub fn advance_frame(&mut self, frame: u64) -> Vec<u32> {
        self.frame = frame;
        let mut reclaimed = Vec::new();
        let mut i = 0;
        while i < self.deferred_free.len() {
            let (slot, due) = self.deferred_free[i];
            if due <= frame {
                self.deferred_free.swap_remove(i);
                let slot = slot as usize;
                self.generations[slot] = self.generations[slot].wrapping_add(1);
                self.states[slot] = SlotState::Free;
                self.free.push(slot as u32);
                reclaimed.push(slot as u32);
            } else {
                i += 1;
            }
        }
        reclaimed
    }

    /// Number of frames a free is deferred by.
    pub fn deferred_frames(&self) -> u64 {
        self.deferred_frames
    }

    /// Current CPU frame.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Explicitly bump the mutation version (used by the GPU half when the
    /// caller overwrites an element's bytes in place).
    pub fn bump_version(&mut self) -> u64 {
        self.version += 1;
        self.version
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{LayoutBuilder, ty};

    fn layout() -> StructLayout {
        LayoutBuilder::new("Item")
            .field("value", ty::F32)
            .build()
            .unwrap()
    }

    #[test]
    fn alloc_then_resolve() {
        let mut pool = DataPool::new(layout(), 4);
        let h0 = pool.alloc().unwrap();
        let ptr = pool.resolve(h0).unwrap();
        assert_eq!(ptr.offset, 0);
        assert_eq!(ptr.size, 4);
        assert_eq!(pool.live_count(), 1);
        assert_eq!(pool.version(), 1);
    }

    #[test]
    fn slots_are_sparse_with_stride() {
        let mut pool = DataPool::new(layout(), 4);
        let h0 = pool.alloc().unwrap();
        let _h1 = pool.alloc().unwrap();
        assert_eq!(pool.resolve(h0).unwrap().offset, 0);
        // Element stride is align_up(4, 16) == 16.
        assert_eq!(pool.resolve(Handle::new(1, 0)).unwrap().offset, 16);
    }

    #[test]
    fn pool_full_error() {
        let mut pool = DataPool::new(layout(), 2);
        let _ = pool.alloc().unwrap();
        let _ = pool.alloc().unwrap();
        let err = pool.alloc().unwrap_err();
        assert_eq!(err, PoolError::PoolFull { capacity: 2 });
    }

    #[test]
    fn free_is_deferred_not_immediate() {
        let mut pool = DataPool::with_deferred_frames(layout(), 4, 2);
        let h0 = pool.alloc().unwrap();
        pool.free(h0).unwrap();
        // Still not reusable, even though the element is no longer live.
        assert!(!pool.is_live(h0));
        // 4 capacity - 1 retired slot = 3 immediately reusable.
        assert_eq!(pool.free_count(), 3);
        let h1 = pool.alloc().unwrap();
        assert_ne!(h1.slot, h0.slot, "retired slot must not be reused yet");
    }

    #[test]
    fn deferred_free_reclaims_after_frames() {
        let mut pool = DataPool::with_deferred_frames(layout(), 4, 2);
        let h0 = pool.alloc().unwrap();
        pool.free(h0).unwrap();

        pool.advance_frame(1);
        assert!(!pool.is_live(h0));
        let h1 = pool.alloc().unwrap();
        assert_ne!(h1.slot, h0.slot, "due_frame is frame+2, not reached yet");

        pool.advance_frame(2);
        let h2 = pool.alloc().unwrap();
        assert_eq!(h2.slot, h0.slot, "slot reused after retirement window");
        // Generation bumped: the old handle is now stale.
        assert!(!pool.is_live(h0));
        assert!(pool.is_live(h2));
        let stale = pool.resolve(h0).unwrap_err();
        assert!(matches!(stale, PoolError::StaleGeneration { .. }));
    }

    #[test]
    fn stale_handle_rejected_after_reuse() {
        let mut pool = DataPool::with_deferred_frames(layout(), 4, 0);
        let h0 = pool.alloc().unwrap();
        pool.free(h0).unwrap();
        pool.advance_frame(1);
        let h1 = pool.alloc().unwrap();
        assert_eq!(h1.slot, h0.slot);
        // Old handle must not resolve to the new occupant.
        assert_eq!(
            pool.resolve(h0).unwrap_err(),
            PoolError::StaleGeneration {
                handle_gen: h0.generation,
                slot_gen: h1.generation,
            }
        );
    }

    #[test]
    fn double_free_is_an_error() {
        let mut pool = DataPool::new(layout(), 4);
        let h0 = pool.alloc().unwrap();
        pool.free(h0).unwrap();
        assert_eq!(pool.free(h0).unwrap_err(), PoolError::NotLive);
    }

    #[test]
    fn out_of_bounds_handle_rejected() {
        let pool = DataPool::new(layout(), 4);
        let err = pool.resolve(Handle::new(99, 0)).unwrap_err();
        assert_eq!(
            err,
            PoolError::OutOfBounds {
                slot: 99,
                capacity: 4
            }
        );
    }

    #[test]
    fn version_bumps_on_mutations() {
        let mut pool = DataPool::new(layout(), 4);
        let v0 = pool.version();
        let h0 = pool.alloc().unwrap();
        assert!(pool.version() > v0);
        pool.bump_version();
        assert!(pool.version() > v0 + 1);
        pool.free(h0).unwrap();
        assert!(pool.version() > v0 + 2);
    }
}
