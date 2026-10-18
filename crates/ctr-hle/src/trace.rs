//! What `Horizon::trace` asks to be logged, by area. A service or the kernel
//! notes a detail only when its flag is set:
//! `if h.trace & trace::GX != 0 { h.note(..) }`. `ctr-diag --trace=gx,ro`
//! sets them.

pub const SVC: u32 = 1 << 0;
pub const IPC: u32 = 1 << 1;
pub const GX: u32 = 1 << 2;
pub const IRQ: u32 = 1 << 3;
pub const DSP: u32 = 1 << 4;
pub const RO: u32 = 1 << 5;
pub const FS: u32 = 1 << 6;
pub const SERVICE: u32 = 1 << 7;
pub const ALL: u32 = !0;

/// The flag called `name`, as `--trace` spells it.
pub fn flag(name: &str) -> Option<u32> {
    Some(match name {
        "svc" => SVC,
        "ipc" => IPC,
        "gx" => GX,
        "irq" => IRQ,
        "dsp" => DSP,
        "ro" => RO,
        "fs" => FS,
        "service" => SERVICE,
        "all" => ALL,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_has_its_own_flag() {
        let names = ["svc", "ipc", "gx", "irq", "dsp", "ro", "fs", "service"];
        let flags: Vec<u32> = names.iter().map(|n| flag(n).unwrap()).collect();
        assert_eq!(flags.iter().fold(0, |all, f| all | f).count_ones(), 8);
        assert_eq!(flag("all"), Some(ALL));
        assert_eq!(flag("nothing"), None);
    }
}
