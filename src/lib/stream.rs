use std::io::{Read, Write, Result as IoResult};

pub const MAGIC: &[u8] = b"GENC";
pub const VERSION: u8 = 3;

pub const FLAG_ENTROPY:  u8 = 0b00000001;
pub const FLAG_CONTEXT:  u8 = 0b00000010;
pub const FLAG_ADAPTIVE: u8 = 0b00000100;
pub const FLAG_LZ77:     u8 = 0b00001000; // LZ77 preprocessing applied before G encoding

#[derive(Debug, Clone)]
pub struct ChunkHeader {
    pub n: u16,
    pub block_count: u64,
    pub compressed_size: u64,
    pub original_bytes: u64, // exact input byte count for this chunk
}

#[derive(Debug)]
pub struct FrameHeader {
    pub flags: u8,
    pub chunk_count: u32,
    pub chunks: Vec<ChunkHeader>,
}

impl FrameHeader {
    pub fn new(flags: u8) -> Self {
        Self { flags, chunk_count: 0, chunks: Vec::new() }
    }

    pub fn add_chunk(&mut self, n: u16, block_count: u64, compressed_size: u64, original_bytes: u64) {
        self.chunks.push(ChunkHeader { n, block_count, compressed_size, original_bytes });
        self.chunk_count += 1;
    }

    pub fn write<W: Write>(&self, w: &mut W) -> IoResult<()> {
        w.write_all(MAGIC)?;
        w.write_all(&[VERSION, self.flags])?;
        w.write_all(&self.chunk_count.to_be_bytes())?;
        for chunk in &self.chunks {
            w.write_all(&chunk.n.to_be_bytes())?;
            w.write_all(&chunk.block_count.to_be_bytes())?;
            w.write_all(&chunk.compressed_size.to_be_bytes())?;
            w.write_all(&chunk.original_bytes.to_be_bytes())?;
        }
        Ok(())
    }

    pub fn read<R: Read>(r: &mut R) -> IoResult<Self> {
        let mut magic = [0u8; 4];
        r.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                "Not a G file (bad magic bytes)"));
        }
        let mut ver_flags = [0u8; 2];
        r.read_exact(&mut ver_flags)?;
        if ver_flags[0] != VERSION {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData,
                format!("Unsupported G version {}. Re-encode with current g.", ver_flags[0])));
        }
        let flags = ver_flags[1];
        let mut cc_buf = [0u8; 4];
        r.read_exact(&mut cc_buf)?;
        let chunk_count = u32::from_be_bytes(cc_buf);
        let mut chunks = Vec::with_capacity(chunk_count as usize);
        for _ in 0..chunk_count {
            let mut nb  = [0u8; 2]; r.read_exact(&mut nb)?;
            let mut bcb = [0u8; 8]; r.read_exact(&mut bcb)?;
            let mut csb = [0u8; 8]; r.read_exact(&mut csb)?;
            let mut obb = [0u8; 8]; r.read_exact(&mut obb)?;
            chunks.push(ChunkHeader {
                n: u16::from_be_bytes(nb),
                block_count: u64::from_be_bytes(bcb),
                compressed_size: u64::from_be_bytes(csb),
                original_bytes: u64::from_be_bytes(obb),
            });
        }
        Ok(Self { flags, chunk_count, chunks })
    }

    pub fn byte_size(&self) -> usize {
        4 + 1 + 1 + 4 + self.chunks.len() * (2 + 8 + 8 + 8)
    }
}

/// Bit-level writer — packs bits MSB-first into bytes
pub struct BitWriter {
    buf: Vec<u8>,
    bit_pos: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self { buf: vec![0], bit_pos: 0 }
    }

    pub fn write_bits(&mut self, value: u128, bits: u32) {
        for i in (0..bits).rev() {
            let bit = ((value >> i) & 1) as u8;
            let last = self.buf.len() - 1;
            self.buf[last] |= bit << (7 - self.bit_pos);
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.buf.push(0);
                self.bit_pos = 0;
            }
        }
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.bit_pos == 0 && self.buf.len() > 1 {
            self.buf.pop();
        }
        self.buf
    }
}

/// Bit-level reader
pub struct BitReader {
    data: Vec<u8>,
    byte_pos: usize,
    bit_pos: u8,
}

impl BitReader {
    pub fn new(data: Vec<u8>) -> Self {
        Self { data, byte_pos: 0, bit_pos: 0 }
    }

    pub fn read_bits(&mut self, bits: u32) -> Option<u128> {
        if bits == 0 { return Some(0); }
        // Check if enough bits remain
        let bits_remaining = (self.data.len() - self.byte_pos) * 8 - self.bit_pos as usize;
        if bits_remaining < bits as usize { return None; }
        let mut value: u128 = 0;
        for _ in 0..bits {
            let bit = (self.data[self.byte_pos] >> (7 - self.bit_pos)) & 1;
            value = (value << 1) | bit as u128;
            self.bit_pos += 1;
            if self.bit_pos == 8 {
                self.bit_pos = 0;
                self.byte_pos += 1;
            }
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let mut h = FrameHeader::new(FLAG_ENTROPY | FLAG_CONTEXT);
        h.add_chunk(28, 1000, 512, 2048);
        let mut buf = Vec::new();
        h.write(&mut buf).unwrap();
        let h2 = FrameHeader::read(&mut buf.as_slice()).unwrap();
        assert_eq!(h2.flags, FLAG_ENTROPY | FLAG_CONTEXT);
        assert_eq!(h2.chunks[0].n, 28);
        assert_eq!(h2.chunks[0].original_bytes, 2048);
    }

    #[test]
    fn bit_roundtrip() {
        let mut w = BitWriter::new();
        w.write_bits(1252, 14);
        w.write_bits(0, 14);
        w.write_bits(14392, 14);
        let data = w.finish();
        let mut r = BitReader::new(data);
        assert_eq!(r.read_bits(14), Some(1252));
        assert_eq!(r.read_bits(14), Some(0));
        assert_eq!(r.read_bits(14), Some(14392));
    }

    #[test]
    fn partial_byte_read() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        w.write_bits(0b11001, 5);
        let data = w.finish();
        let mut r = BitReader::new(data);
        assert_eq!(r.read_bits(3), Some(0b101));
        assert_eq!(r.read_bits(5), Some(0b11001));
        assert_eq!(r.read_bits(1), None); // no more data
    }
}
