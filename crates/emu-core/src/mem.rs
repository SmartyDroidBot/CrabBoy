//! Fixed-size, heap-allocated memory regions.

use std::ops::{Deref, DerefMut, Index, IndexMut};

/// A fixed-size memory region, heap-allocated with no stack temporary.
///
/// Large regions must not be embedded inline in the emulator structs: in
/// debug builds Rust materialises a returned value as a temporary on the
/// caller's stack, which overflows the default thread stack for anything
/// approaching a megabyte (and the 3DS has a 128 MB region). Building the
/// region as a `Vec` and converting the boxed slice allocates the `[T; N]`
/// directly on the heap while keeping the exact size in the type.
pub struct Mem<T, const N: usize>(Box<[T; N]>);

impl<T: Copy + Default, const N: usize> Mem<T, N> {
    /// Allocate a zero-initialised region directly on the heap.
    pub fn zeroed() -> Self {
        Self::filled(T::default())
    }

    /// Allocate a region filled with a repeated value.
    pub fn filled(v: T) -> Self {
        let boxed: Box<[T]> = vec![v; N].into_boxed_slice();
        let Ok(array) = boxed.try_into() else {
            unreachable!("boxed slice has exactly N elements")
        };
        Mem(array)
    }
}

impl<T, const N: usize> Index<usize> for Mem<T, N> {
    type Output = T;
    #[inline]
    fn index(&self, i: usize) -> &T {
        &self.0[i]
    }
}

impl<T, const N: usize> IndexMut<usize> for Mem<T, N> {
    #[inline]
    fn index_mut(&mut self, i: usize) -> &mut T {
        &mut self.0[i]
    }
}

macro_rules! impl_index_range {
    ($($r:ty),+ $(,)?) => {$(
        impl<T, const N: usize> Index<$r> for Mem<T, N> {
            type Output = [T];
            #[inline]
            fn index(&self, i: $r) -> &[T] {
                &self.0[i]
            }
        }
        impl<T, const N: usize> IndexMut<$r> for Mem<T, N> {
            #[inline]
            fn index_mut(&mut self, i: $r) -> &mut [T] {
                &mut self.0[i]
            }
        }
    )+};
}

impl_index_range!(
    std::ops::Range<usize>,
    std::ops::RangeFrom<usize>,
    std::ops::RangeTo<usize>,
    std::ops::RangeFull,
    std::ops::RangeInclusive<usize>,
);

impl<T, const N: usize> Deref for Mem<T, N> {
    type Target = [T; N];
    fn deref(&self) -> &[T; N] {
        &self.0
    }
}

impl<T, const N: usize> DerefMut for Mem<T, N> {
    fn deref_mut(&mut self) -> &mut [T; N] {
        &mut self.0
    }
}

impl<T, const N: usize> AsRef<[T]> for Mem<T, N> {
    fn as_ref(&self) -> &[T] {
        &self.0[..]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_region_allocates_without_touching_the_stack() {
        let mut m: Mem<u8, { 8 << 20 }> = Mem::zeroed();
        assert_eq!(m.len(), 8 << 20);
        m[(8 << 20) - 1] = 0xAB;
        assert_eq!(m[(8 << 20) - 1..], [0xAB]);
    }

    #[test]
    fn filled_repeats_the_value() {
        let m: Mem<u16, 4> = Mem::filled(0xFFFF);
        assert_eq!(*m, [0xFFFF; 4]);
    }
}
