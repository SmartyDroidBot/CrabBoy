//! A record of accesses to registers that are not modelled, for the
//! diagnostic tools. Without the `trace` feature it records nothing.

#[cfg(feature = "trace")]
use std::collections::BTreeMap;

/// One unmodelled register and how it was used.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Entry {
    pub reads: u64,
    pub writes: u64,
    /// The last value written, in its byte lanes.
    pub last_write: u32,
}

#[derive(Default)]
pub struct Trace {
    #[cfg(feature = "trace")]
    entries: BTreeMap<u32, Entry>,
}

impl Trace {
    /// Note a read; unmodelled registers read as zero.
    #[inline]
    pub fn read(&mut self, _addr: u32) -> u32 {
        #[cfg(feature = "trace")]
        {
            self.entries.entry(_addr).or_default().reads += 1;
        }
        0
    }

    #[inline]
    pub fn write(&mut self, _addr: u32, _value: u32, _mask: u32) {
        #[cfg(feature = "trace")]
        {
            let entry = self.entries.entry(_addr).or_default();
            entry.writes += 1;
            entry.last_write = entry.last_write & !_mask | _value & _mask;
        }
    }

    /// Every unmodelled register touched so far, by address.
    #[cfg(feature = "trace")]
    pub fn entries(&self) -> impl Iterator<Item = (u32, Entry)> + '_ {
        self.entries.iter().map(|(addr, entry)| (*addr, *entry))
    }
}
