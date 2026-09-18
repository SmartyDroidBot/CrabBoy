//! Little-endian save-state serialisation shared by the cores.
//!
//! Every variable-length field is `u32`-length-prefixed so [`Reader`] can
//! bounds-check it; a truncated or corrupt state is an `Err`, never a panic.

/// Little-endian serialiser.
#[derive(Default)]
pub struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    pub fn new() -> Self {
        Writer::default()
    }

    /// Start a state with a 4-byte magic and a format version.
    pub fn with_header(magic: &[u8; 4], version: u32) -> Self {
        let mut w = Writer::new();
        w.buf.extend_from_slice(magic);
        w.u32(version);
        w
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }

    /// Length-prefixed bytes.
    pub fn bytes(&mut self, b: &[u8]) {
        self.u32(b.len() as u32);
        self.buf.extend_from_slice(b);
    }

    /// Raw bytes with no length prefix (read back with [`Reader::array`]).
    pub fn raw(&mut self, b: &[u8]) {
        self.buf.extend_from_slice(b);
    }

    pub fn finish(self) -> Vec<u8> {
        self.buf
    }
}

/// Bounds-checked reader matching [`Writer`].
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    /// Check the magic written by [`Writer::with_header`] and return the
    /// format version that follows it.
    pub fn with_header(data: &'a [u8], magic: &[u8; 4]) -> Result<(Self, u32), String> {
        let mut r = Reader::new(data);
        if &r.array::<4>()? != magic {
            return Err("not a save state for this system".to_string());
        }
        let version = r.u32()?;
        Ok((r, version))
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(len).ok_or("unexpected end of state")?;
        let b = self
            .data
            .get(self.pos..end)
            .ok_or("unexpected end of state")?;
        self.pos = end;
        Ok(b)
    }

    pub fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub fn bool(&mut self) -> Result<bool, String> {
        Ok(self.u8()? != 0)
    }

    /// Exactly `N` raw bytes (no length prefix).
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    /// Length-prefixed bytes, borrowed from the state.
    pub fn bytes(&mut self) -> Result<&'a [u8], String> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    /// Length-prefixed bytes copied into `out`, which must match the stored
    /// length exactly.
    pub fn bytes_into(&mut self, out: &mut [u8]) -> Result<(), String> {
        let b = self.bytes()?;
        if b.len() != out.len() {
            return Err(format!(
                "state field is {} bytes, expected {}",
                b.len(),
                out.len()
            ));
        }
        out.copy_from_slice(b);
        Ok(())
    }

    /// Bytes not consumed yet.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_every_field_kind() {
        let mut w = Writer::with_header(b"TEST", 3);
        w.u8(1);
        w.u16(0x1234);
        w.u32(0xDEAD_BEEF);
        w.u64(0x0123_4567_89AB_CDEF);
        w.bool(true);
        w.bytes(&[9, 8, 7]);
        w.raw(&[5, 6]);
        let data = w.finish();

        let (mut r, version) = Reader::with_header(&data, b"TEST").unwrap();
        assert_eq!(version, 3);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.u64().unwrap(), 0x0123_4567_89AB_CDEF);
        assert!(r.bool().unwrap());
        assert_eq!(r.bytes().unwrap(), [9, 8, 7]);
        assert_eq!(r.array::<2>().unwrap(), [5, 6]);
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn rejects_a_wrong_magic_and_truncated_data() {
        let data = Writer::with_header(b"TEST", 1).finish();
        assert!(Reader::with_header(&data, b"NOPE").is_err());
        assert!(Reader::with_header(&data[..6], b"TEST").is_err());

        let mut w = Writer::new();
        w.u32(1000);
        let data = w.finish();
        assert!(Reader::new(&data).bytes().is_err());
        assert!(Reader::new(&[0xFF; 4]).bytes().is_err());
    }

    #[test]
    fn bytes_into_checks_the_length() {
        let mut w = Writer::new();
        w.bytes(&[1, 2, 3]);
        let data = w.finish();
        let mut short = [0u8; 2];
        assert!(Reader::new(&data).bytes_into(&mut short).is_err());
        let mut exact = [0u8; 3];
        Reader::new(&data).bytes_into(&mut exact).unwrap();
        assert_eq!(exact, [1, 2, 3]);
    }
}
