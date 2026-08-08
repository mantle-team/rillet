//! Gauges: lock-free latest-value cells for continuous signals.
//!
//! A gauge carries a signal whose history does not matter, like an audio
//! level or a throughput. The producer stores the latest value without
//! locking, allocating, or waking anyone; consumers sample the current
//! value whenever they want it. A service declares one with
//! `#[rillet(gauge)]` on a field, and the generated handle method samples
//! it without touching the service's state lock.
//!
//! Cells clone cheaply and share their storage, so a clone can live inside
//! a latency-sensitive callback, and a view can carry cells for signals
//! whose cardinality follows the view's structure. Cell equality is
//! identity, so a stored value never republishes a view that carries the
//! cell.

use std::marker::PhantomData;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::view::CheapClone;

/// A lock-free latest-value cell: stores are wait-free and
/// allocation-free, loads are wait-free.
///
/// The built-in cells guarantee this. A hand-written impl carries no such
/// check.
pub trait GaugeCell: Clone {
    type Value;

    /// Replaces the current value.
    fn store(&self, value: Self::Value);

    /// Returns the current value.
    fn load(&self) -> Self::Value;
}

/// A value representable as one machine word.
///
/// Implemented for the primitive numerics and `bool`; implement it to fit
/// another `Copy` type into an [`Atomic`] cell.
pub trait GaugeValue: Copy {
    fn to_bits(self) -> u64;
    fn from_bits(bits: u64) -> Self;
}

macro_rules! impl_gauge_value_int {
    ($($ty:ty),* $(,)?) => {
        $(impl GaugeValue for $ty {
            fn to_bits(self) -> u64 {
                self as u64
            }

            fn from_bits(bits: u64) -> Self {
                bits as $ty
            }
        })*
    };
}

impl_gauge_value_int!(u8, u16, u32, u64, usize, i8, i16, i32, i64, isize);

impl GaugeValue for f32 {
    fn to_bits(self) -> u64 {
        u64::from(self.to_bits())
    }

    fn from_bits(bits: u64) -> Self {
        f32::from_bits(bits as u32)
    }
}

impl GaugeValue for f64 {
    fn to_bits(self) -> u64 {
        self.to_bits()
    }

    fn from_bits(bits: u64) -> Self {
        f64::from_bits(bits)
    }
}

impl GaugeValue for bool {
    fn to_bits(self) -> u64 {
        u64::from(self)
    }

    fn from_bits(bits: u64) -> Self {
        bits != 0
    }
}

/// A gauge cell holding one word-sized value.
///
/// Clones share the cell. Equality is cell identity, not the current
/// value.
pub struct Atomic<T> {
    bits: Arc<AtomicU64>,
    _value: PhantomData<T>,
}

impl<T: GaugeValue> Atomic<T> {
    /// Creates a cell holding an initial value.
    pub fn new(initial: T) -> Self {
        Self {
            bits: Arc::new(AtomicU64::new(initial.to_bits())),
            _value: PhantomData,
        }
    }

    /// Replaces the current value.
    pub fn store(&self, value: T) {
        // Relaxed: the value is complete in its one word; no other memory
        // is published with it.
        self.bits.store(value.to_bits(), Ordering::Relaxed);
    }

    /// Returns the current value.
    pub fn load(&self) -> T {
        T::from_bits(self.bits.load(Ordering::Relaxed))
    }
}

impl<T: GaugeValue> GaugeCell for Atomic<T> {
    type Value = T;

    fn store(&self, value: T) {
        Atomic::store(self, value);
    }

    fn load(&self) -> T {
        Atomic::load(self)
    }
}

impl<T> Clone for Atomic<T> {
    fn clone(&self) -> Self {
        Self {
            bits: self.bits.clone(),
            _value: PhantomData,
        }
    }
}

impl<T> CheapClone for Atomic<T> {}

impl<T: GaugeValue + Default> Default for Atomic<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T: GaugeValue + std::fmt::Debug> std::fmt::Debug for Atomic<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Atomic").field(&self.load()).finish()
    }
}

impl<T> PartialEq for Atomic<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.bits, &other.bits)
    }
}

impl<T> Eq for Atomic<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_returns_the_latest_store() {
        let cell = Atomic::new(0.0f32);
        cell.store(0.75);
        assert_eq!(cell.load(), 0.75);
    }

    #[test]
    fn negative_values_roundtrip() {
        let float = Atomic::new(0.0f64);
        float.store(-3.5);
        assert_eq!(float.load(), -3.5);

        let int = Atomic::new(0i32);
        int.store(-42);
        assert_eq!(int.load(), -42);
    }

    #[test]
    fn clones_share_the_cell() {
        let cell = Atomic::new(0u64);
        let writer = cell.clone();
        writer.store(7);
        assert_eq!(cell.load(), 7);
    }

    #[test]
    fn equality_is_cell_identity() {
        let cell = Atomic::new(1u32);
        let same = Atomic::new(1u32);
        assert_eq!(cell, cell.clone());
        assert_ne!(cell, same);
    }

    #[test]
    fn default_holds_the_value_default() {
        assert_eq!(Atomic::<f32>::default().load(), 0.0);
        assert!(!Atomic::<bool>::default().load());
    }
}
