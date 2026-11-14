//! A recycling pool for in-memory buffers.
//!
//! Hot paths that shuttle buffers around (packet processing, serialization
//! scratch space) can spend more time in the allocator than on the work
//! itself. [`BufferPool`] keeps a bounded set of `Vec<u8>` allocations
//! around so `acquire`/`drop` cycles reuse them instead of re-allocating.
//!
//! ```
//! use cachegirl::BufferPool;
//!
//! let pool = BufferPool::new();
//! {
//!     let mut buf = pool.acquire_with_capacity(4096);
//!     buf.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
//!     assert_eq!(buf.len(), 4);
//!     // dropping the handle returns the allocation to the pool
//! }
//! assert_eq!(pool.pooled(), 1);
//! let reused = pool.acquire();
//! assert!(reused.capacity() >= 4096);
//! ```

use std::sync::{Arc, Mutex, MutexGuard};

use crate::resource::Buffer;

/// Default number of buffers a pool keeps around for reuse.
const DEFAULT_MAX_BUFFERS: usize = 64;
/// Default cap on the capacity of a pooled buffer (1 MiB).
const DEFAULT_MAX_BUFFER_CAPACITY: usize = 1024 * 1024;

/// A thread-safe pool of reusable byte buffers.
///
/// Checking a buffer out is an `acquire`; dropping the returned
/// [`PooledBuffer`] clears it (keeping the allocation) and — provided it
/// passes the configured limits — returns it to the pool. Buffers that
/// outgrew the pool's size cap, or buffers arriving at a full pool, are
/// dropped instead of retained.
///
/// ## Quotas and namespaces
///
/// A [`BufferPool`] is deliberately not tied to a namespace, a manager or
/// the quota system. Pooled buffers are not registered resources: they
/// carry no registry record, answer no cleanup policy and consume no
/// quota. That is the whole point — the pool exists for call sites where
/// even a registry lookup is overhead worth avoiding, and quota accounting
/// in this crate is built on registry records. Checked-in buffers stay
/// allocated (that is what makes them reusable), so charging quota for
/// them would permanently eat it or require quota-release plumbing on a
/// `Drop` that cannot report failure.
///
/// If you need accounted, policy-managed buffers, use
/// `Namespace::create_buffer`; the pool is its unaccounted sibling for
/// scratch space. If you need both, use the pool for transient work and
/// create a tracked resource only for data that escapes.
///
/// Clone the pool to share it: clones point at the same arena.
#[derive(Debug)]
pub struct BufferPool {
    inner: Arc<Mutex<PoolInner>>,
}

impl Clone for BufferPool {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Default for BufferPool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
struct PoolInner {
    free: Vec<Buffer>,
    max_buffers: usize,
    max_buffer_capacity: usize,
}

impl BufferPool {
    /// Pool with the default limits (64 buffers, 1 MiB of capacity each).
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_BUFFERS, DEFAULT_MAX_BUFFER_CAPACITY)
    }

    /// Pool with explicit limits.
    ///
    /// `max_buffers` bounds how many idle buffers are retained;
    /// `max_buffer_capacity` bounds the capacity of a retained buffer.
    /// Both accept zero: a pool with `max_buffers == 0` never retains
    /// anything (every `acquire` allocates), and `max_buffer_capacity == 0`
    /// retains only allocation-free buffers.
    pub fn with_limits(max_buffers: usize, max_buffer_capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(PoolInner {
                free: Vec::new(),
                max_buffers,
                max_buffer_capacity,
            })),
        }
    }

    /// Maximum number of idle buffers retained.
    pub fn max_buffers(&self) -> usize {
        self.lock().max_buffers
    }

    /// Maximum capacity of a retained buffer.
    pub fn max_buffer_capacity(&self) -> usize {
        self.lock().max_buffer_capacity
    }

    /// Number of buffers currently checked in and reusable.
    pub fn pooled(&self) -> usize {
        self.lock().free.len()
    }

    /// Drop every pooled buffer without waiting for a checkout cycle.
    pub fn clear(&self) {
        self.lock().free.clear();
    }

    /// Take a cleared buffer with whatever capacity the pool has (zero
    /// when the pool is empty).
    pub fn acquire(&self) -> PooledBuffer {
        self.acquire_with_capacity(0)
    }

    /// Take a cleared buffer with at least `capacity` bytes of capacity.
    ///
    /// The pool hands out the smallest buffer that fits, so tight fits
    /// are not wasted on oversized requests. If nothing fits, the
    /// smallest idle buffer is grown to the requested capacity instead
    /// of being stranded in the pool forever; a fresh buffer is
    /// allocated when the pool has nothing usable at all.
    pub fn acquire_with_capacity(&self, capacity: usize) -> PooledBuffer {
        let mut inner = self.lock();
        // smallest buffer that fits the request, if any
        let fit = inner
            .free
            .iter()
            .enumerate()
            .filter(|(_, b)| b.capacity() >= capacity)
            .min_by_key(|(_, b)| b.capacity())
            .map(|(i, _)| i);
        let buffer = if let Some(i) = fit {
            inner.free.remove(i)
        } else if capacity > 0 && !inner.free.is_empty() {
            // nothing big enough: grow the smallest idle buffer rather
            // than leaving it unusable forever
            let i = inner
                .free
                .iter()
                .enumerate()
                .min_by_key(|(_, b)| b.capacity())
                .map(|(i, _)| i)
                .expect("free list is not empty");
            let mut buffer = inner.free.remove(i);
            buffer.reserve_exact(capacity);
            buffer
        } else if capacity == 0 {
            Buffer::default()
        } else {
            Buffer::with_capacity(capacity)
        };
        let mut buffer = buffer;
        buffer.clear();
        PooledBuffer {
            pool: BufferPool {
                inner: Arc::clone(&self.inner),
            },
            buffer: Some(buffer),
        }
    }

    fn lock(&self) -> MutexGuard<'_, PoolInner> {
        // a poisoned lock means a thread panicked while holding it; the
        // buffers inside are plain data, so recover and keep going —
        // this must never take a hot path down
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// A buffer checked out of a [`BufferPool`].
///
/// Dropping it clears the buffer and returns the allocation to the pool
/// (subject to the pool's limits). [`PooledBuffer::into_vec`] extracts
/// the allocation and shuts pooling out for this buffer.
#[derive(Debug)]
pub struct PooledBuffer {
    pool: BufferPool,
    buffer: Option<Buffer>,
}

impl PooledBuffer {
    /// Borrow the contents.
    pub fn as_slice(&self) -> &[u8] {
        self.buffer.as_ref().map(Buffer::as_slice).unwrap_or(&[])
    }

    /// Borrow the contents mutably.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.buffer
            .as_mut()
            .map(Buffer::as_mut_slice)
            .unwrap_or(&mut [])
    }

    /// Number of bytes currently held.
    pub fn len(&self) -> usize {
        self.buffer.as_ref().map_or(0, Buffer::len)
    }

    /// True when no bytes are held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Allocated capacity.
    pub fn capacity(&self) -> usize {
        self.buffer.as_ref().map_or(0, Buffer::capacity)
    }

    /// Append bytes, growing as needed.
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        if let Some(buffer) = self.buffer.as_mut() {
            buffer.extend_from_slice(bytes);
        }
    }

    /// Forget the contents but keep the allocation for reuse.
    pub fn clear(&mut self) {
        if let Some(buffer) = self.buffer.as_mut() {
            buffer.clear();
        }
    }

    /// Consume the handle and return the inner vec.
    ///
    /// The allocation leaves the pool's recycling cycle for good.
    pub fn into_vec(mut self) -> Vec<u8> {
        let buffer = self
            .buffer
            .take()
            .expect("pool buffer checked out exactly once");
        buffer.into_inner()
    }
}

impl AsRef<[u8]> for PooledBuffer {
    fn as_ref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Drop for PooledBuffer {
    fn drop(&mut self) {
        let Some(mut buffer) = self.buffer.take() else {
            return;
        };
        buffer.clear();
        let mut inner = self.pool.lock();
        if buffer.capacity() <= inner.max_buffer_capacity && inner.free.len() < inner.max_buffers {
            inner.free.push(buffer);
        }
        // else: too big to keep, or the pool is already full — let the
        // allocator have it back
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Barrier, Mutex};

    #[test]
    fn fresh_acquire_is_empty_with_zero_capacity() {
        let pool = BufferPool::with_limits(4, 1024);
        let buf = pool.acquire();
        assert!(buf.is_empty());
        assert_eq!(buf.capacity(), 0);
        assert_eq!(pool.pooled(), 0);
    }

    #[test]
    fn returned_buffer_keeps_capacity_and_allocation() {
        let pool = BufferPool::with_limits(4, 8192);
        let ptr_before;
        let capacity_before;
        {
            let mut buf = pool.acquire_with_capacity(1024);
            buf.extend_from_slice(&[0xAB; 512]);
            ptr_before = buf.as_slice().as_ptr();
            capacity_before = buf.capacity();
        }
        assert_eq!(pool.pooled(), 1);
        let reused = pool.acquire();
        assert_eq!(reused.capacity(), capacity_before);
        assert_eq!(reused.as_slice().as_ptr(), ptr_before);
        assert!(reused.is_empty(), "buffer must be cleared before reuse");
    }

    #[test]
    fn acquire_with_capacity_reuses_the_smallest_fit() {
        let pool = BufferPool::with_limits(8, 8192);
        let small_ptr;
        let big_ptr;
        {
            let small = pool.acquire_with_capacity(512);
            let big = pool.acquire_with_capacity(1024);
            small_ptr = small.as_slice().as_ptr();
            big_ptr = big.as_slice().as_ptr();
        }
        assert_eq!(pool.pooled(), 2);
        {
            let buf = pool.acquire_with_capacity(600);
            assert_eq!(
                buf.as_slice().as_ptr(),
                big_ptr,
                "only the 1024 buffer fits 600"
            );
        }
        {
            let buf = pool.acquire_with_capacity(100);
            assert_eq!(buf.as_slice().as_ptr(), small_ptr, "smallest fit for 100");
        }
    }

    #[test]
    fn no_fitting_buffer_grows_the_smallest_idle_one() {
        let pool = BufferPool::with_limits(4, 8192);
        {
            let small = pool.acquire_with_capacity(512);
            drop(small);
        }
        let grown = pool.acquire_with_capacity(2048);
        assert!(grown.capacity() >= 2048);
        assert_eq!(
            pool.pooled(),
            0,
            "the idle buffer was taken, not the fresh path"
        );
    }

    #[test]
    fn pool_respects_max_buffers() {
        let pool = BufferPool::with_limits(2, 8192);
        let a = pool.acquire_with_capacity(128);
        let b = pool.acquire_with_capacity(128);
        let c = pool.acquire_with_capacity(128);
        drop(a);
        drop(b);
        drop(c);
        assert_eq!(pool.pooled(), 2);
    }

    #[test]
    fn oversized_buffer_is_dropped_not_pooled() {
        let pool = BufferPool::with_limits(8, 64);
        let big = pool.acquire_with_capacity(128);
        drop(big);
        assert_eq!(pool.pooled(), 0);
    }

    #[test]
    fn capacity_at_the_limit_is_retained() {
        let pool = BufferPool::with_limits(8, 64);
        let exact = pool.acquire_with_capacity(64);
        drop(exact);
        assert_eq!(pool.pooled(), 1);
    }

    #[test]
    fn zero_max_buffers_pools_nothing() {
        let pool = BufferPool::with_limits(0, 8192);
        let buf = pool.acquire_with_capacity(1024);
        drop(buf);
        assert_eq!(pool.pooled(), 0);
        let fresh = pool.acquire();
        assert_eq!(fresh.capacity(), 0);
    }

    #[test]
    fn zero_capacity_buffers_are_retained_with_zero_byte_cap() {
        let pool = BufferPool::with_limits(4, 0);
        let buf = pool.acquire();
        let ptr = buf.as_slice().as_ptr();
        drop(buf);
        assert_eq!(pool.pooled(), 1, "a zero-allocation buffer fits a zero cap");
        let reused = pool.acquire();
        assert_eq!(reused.as_slice().as_ptr(), ptr);
    }

    #[test]
    fn into_vec_escapes_the_recycling_cycle() {
        let pool = BufferPool::with_limits(4, 8192);
        let mut buf = pool.acquire_with_capacity(256);
        buf.extend_from_slice(b"keep me");
        let vec = buf.into_vec();
        assert_eq!(vec, b"keep me");
        assert_eq!(pool.pooled(), 0, "escaped allocation must not come back");
    }

    #[test]
    fn clear_drops_all_pooled_buffers() {
        let pool = BufferPool::with_limits(4, 8192);
        let a = pool.acquire_with_capacity(128);
        let b = pool.acquire_with_capacity(128);
        drop(a);
        drop(b);
        assert_eq!(pool.pooled(), 2);
        pool.clear();
        assert_eq!(pool.pooled(), 0);
    }

    #[test]
    fn cloned_pools_share_the_arena() {
        let pool = BufferPool::with_limits(4, 1024);
        let clone = pool.clone();
        let buf = clone.acquire();
        drop(buf);
        assert_eq!(pool.pooled(), 1);
        assert_eq!(clone.pooled(), 1);
    }

    #[test]
    fn concurrent_acquire_and_return_keep_limits() {
        use std::sync::Arc;
        let pool = Arc::new(BufferPool::with_limits(8, 8192));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let pool = Arc::clone(&pool);
            handles.push(std::thread::spawn(move || {
                for _ in 0..250 {
                    let mut buf = pool.acquire_with_capacity(512);
                    buf.extend_from_slice(&[1, 2, 3, 4]);
                    assert_eq!(buf.len(), 4);
                    drop(buf);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert!(pool.pooled() <= 8);
    }

    #[test]
    fn simultaneous_checkouts_hand_out_distinct_allocations() {
        use std::sync::Arc;
        let pool = Arc::new(BufferPool::with_limits(16, 4096));
        // warm the pool so acquires draw real pooled allocations
        for _ in 0..16 {
            drop(pool.acquire_with_capacity(1024));
        }
        let barrier = Arc::new(Barrier::new(16));
        let seen = Arc::new(Mutex::new(HashSet::new()));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let pool = Arc::clone(&pool);
            let barrier = Arc::clone(&barrier);
            let seen = Arc::clone(&seen);
            handles.push(std::thread::spawn(move || {
                let buf = pool.acquire_with_capacity(1024);
                barrier.wait();
                seen.lock()
                    .unwrap()
                    .insert(buf.as_slice().as_ptr() as usize);
                barrier.wait();
                buf
            }));
        }
        let buffers: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(
            seen.lock().unwrap().len(),
            16,
            "no allocation may be checked out twice"
        );
        drop(buffers);
        assert_eq!(pool.pooled(), 16);
    }
}
