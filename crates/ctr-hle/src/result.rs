//! Result codes (3dbrew, "Error codes"): a description in bits 0-9, the
//! module in bits 10-17, a summary in bits 21-26 and a level in bits 27-31.
//! Zero is success; software mostly tests the sign bit.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ResultCode(pub u32);

pub mod level {
    pub const PERMANENT: u32 = 27;
    pub const USAGE: u32 = 28;
}

pub mod summary {
    pub const OUT_OF_RESOURCE: u32 = 3;
    pub const INVALID_ARGUMENT: u32 = 7;
}

pub mod module {
    pub const KERNEL: u32 = 1;
    pub const OS: u32 = 6;
}

pub mod description {
    pub const MISALIGNED_SIZE: u32 = 1010;
    pub const OUT_OF_MEMORY: u32 = 1011;
    pub const NOT_IMPLEMENTED: u32 = 1012;
    pub const INVALID_ADDRESS: u32 = 1013;
}

pub const fn make(level: u32, summary: u32, module: u32, description: u32) -> ResultCode {
    ResultCode(level << 27 | summary << 21 | module << 10 | description)
}

impl ResultCode {
    pub fn is_error(self) -> bool {
        self.0 >> 31 != 0
    }
}

pub const SUCCESS: ResultCode = ResultCode(0);
pub const OUT_OF_MEMORY: ResultCode = make(
    level::PERMANENT,
    summary::OUT_OF_RESOURCE,
    module::KERNEL,
    description::OUT_OF_MEMORY,
);
pub const MISALIGNED_SIZE: ResultCode = make(
    level::USAGE,
    summary::INVALID_ARGUMENT,
    module::OS,
    description::MISALIGNED_SIZE,
);
pub const INVALID_ADDRESS: ResultCode = make(
    level::USAGE,
    summary::INVALID_ARGUMENT,
    module::OS,
    description::INVALID_ADDRESS,
);
pub const NOT_IMPLEMENTED: ResultCode = make(
    level::USAGE,
    summary::INVALID_ARGUMENT,
    module::OS,
    description::NOT_IMPLEMENTED,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fields_compose_into_the_codes_software_knows() {
        assert_eq!(OUT_OF_MEMORY.0, 0xD860_07F3);
        assert_eq!(MISALIGNED_SIZE.0, 0xE0E0_1BF2);
        assert_eq!(INVALID_ADDRESS.0, 0xE0E0_1BF5);
        assert!(OUT_OF_MEMORY.is_error() && !SUCCESS.is_error());
    }
}
