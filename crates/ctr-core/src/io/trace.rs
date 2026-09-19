//! A record of accesses to registers that are not modelled, for the
//! diagnostic tools. Without the `trace` feature it records nothing.

#[cfg(feature = "trace")]
use std::collections::{BTreeMap, VecDeque};

/// How many watched accesses are kept; older ones are dropped.
#[cfg(feature = "trace")]
const LOG_LEN: usize = 4000;

/// One access to a watched address, in order of occurrence.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Access {
    pub addr: u32,
    pub write: bool,
    pub value: u32,
}

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
    /// Every I/O access since the last [`Trace::take_recent`], modelled or
    /// not.
    #[cfg(feature = "trace")]
    recent: BTreeMap<u32, Entry>,
    /// Address ranges whose accesses are logged in order, ends included.
    #[cfg(feature = "trace")]
    watched: Vec<(u32, u32)>,
    #[cfg(feature = "trace")]
    log: VecDeque<Access>,
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

    /// Note any I/O access, for the recent-activity view.
    #[inline]
    pub fn touch(&mut self, _addr: u32, _write: Option<u32>) {
        #[cfg(feature = "trace")]
        {
            let entry = self.recent.entry(_addr).or_default();
            match _write {
                Some(value) => {
                    entry.writes += 1;
                    entry.last_write = value;
                }
                None => entry.reads += 1,
            }
        }
    }

    /// Note the value of an access to a watched address.
    #[inline]
    pub fn log(&mut self, _addr: u32, _write: bool, _value: u32) {
        #[cfg(feature = "trace")]
        if self
            .watched
            .iter()
            .any(|&(lo, hi)| (lo..=hi).contains(&_addr))
        {
            if self.log.len() == LOG_LEN {
                self.log.pop_front();
            }
            self.log.push_back(Access {
                addr: _addr,
                write: _write,
                value: _value,
            });
        }
    }

    /// Log every access to `lo..=hi` from now on.
    #[cfg(feature = "trace")]
    pub fn watch(&mut self, lo: u32, hi: u32) {
        self.watched.push((lo, hi));
    }

    /// The latest watched accesses, oldest first.
    #[cfg(feature = "trace")]
    pub fn logged(&self) -> impl Iterator<Item = Access> + '_ {
        self.log.iter().copied()
    }

    /// The accesses since the last call, by address.
    #[cfg(feature = "trace")]
    pub fn take_recent(&mut self) -> Vec<(u32, Entry)> {
        std::mem::take(&mut self.recent).into_iter().collect()
    }

    /// Every unmodelled register touched so far, by address.
    #[cfg(feature = "trace")]
    pub fn entries(&self) -> impl Iterator<Item = (u32, Entry)> + '_ {
        self.entries.iter().map(|(addr, entry)| (*addr, *entry))
    }
}
