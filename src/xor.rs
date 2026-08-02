use std::fs;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Read;

pub const XOR_KEY_LEN: usize = 8;

pub type XorKey = [u8; XOR_KEY_LEN];

/// Read the obfuscation key from `xor.dat` in the blocks directory.
/// Bitcoin Core >= 28 XORs blk*.dat files with this 8-byte key.
/// Returns an all-zero key (no obfuscation) if the file does not exist,
/// which is the case for block directories created by older versions.
pub fn read_xor_key(blocks_dir: &std::path::Path) -> Result<XorKey, Error> {
    match fs::read(blocks_dir.join("xor.dat")) {
        Ok(bytes) => bytes
            .try_into()
            .map_err(|_| Error::new(ErrorKind::InvalidData, "xor.dat must be 8 bytes")),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok([0; XOR_KEY_LEN]),
        Err(e) => Err(e),
    }
}

/// XOR a whole buffer in place, one 64-bit word at a time.
/// The buffer must start at position 0 of the file, so that the key
/// alignment is preserved. A zero key leaves the buffer unchanged.
pub fn xor_in_place(buf: &mut [u8], key: XorKey) {
    xor_in_place_at(buf, key, 0);
}

/// Same as [`xor_in_place`] for a buffer starting at an arbitrary
/// position in the file: the key is rotated to match the alignment.
pub fn xor_in_place_at(buf: &mut [u8], key: XorKey, base_offset: u64) {
    if key == [0; XOR_KEY_LEN] {
        return;
    }

    // Rotate the key so that index 0 matches base_offset
    let rotated: XorKey =
        std::array::from_fn(|i| key[(base_offset as usize + i) % XOR_KEY_LEN]);

    let key64 = u64::from_ne_bytes(rotated);
    let mut chunks = buf.chunks_exact_mut(XOR_KEY_LEN);
    for chunk in &mut chunks {
        let word = u64::from_ne_bytes((&*chunk).try_into().unwrap()) ^ key64;
        chunk.copy_from_slice(&word.to_ne_bytes());
    }

    // The remainder starts at a multiple of the key length
    for (i, byte) in chunks.into_remainder().iter_mut().enumerate() {
        *byte ^= rotated[i % XOR_KEY_LEN];
    }
}

/// Reader that de-obfuscates data XORed with an 8-byte key,
/// keyed on the absolute position in the underlying file.
/// An all-zero key leaves the data unchanged.
pub struct XorReader<R> {
    inner: R,
    key: XorKey,
    pos: u64,
}

impl<R: Read> XorReader<R> {
    pub fn new(inner: R, key: XorKey) -> XorReader<R> {
        XorReader::with_offset(inner, key, 0)
    }

    /// Use when `inner` does not start at the beginning of the file
    /// (e.g. after a seek); `offset` is the absolute position in the file.
    pub fn with_offset(inner: R, key: XorKey, offset: u64) -> XorReader<R> {
        XorReader {
            inner,
            key,
            pos: offset,
        }
    }
}

impl<R: Read> Read for XorReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let n = self.inner.read(buf)?;

        if self.key != [0; XOR_KEY_LEN] {
            for byte in &mut buf[..n] {
                *byte ^= self.key[(self.pos % XOR_KEY_LEN as u64) as usize];
                self.pos += 1;
            }
        } else {
            self.pos += n as u64;
        }

        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_reader_applies_key_from_offset() {
        let key: XorKey = [0xde, 0xca, 0x6d, 0x01, 0x00, 0x56, 0x05, 0xe5];
        let plain: Vec<u8> = (0u8..32).collect();
        let obfuscated: Vec<u8> = plain
            .iter()
            .enumerate()
            .map(|(i, b)| b ^ key[i % XOR_KEY_LEN])
            .collect();

        let mut decoded = Vec::new();
        XorReader::new(&obfuscated[..], key)
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, plain);

        // Reading from a position that is not a multiple of the key length
        let mut decoded = Vec::new();
        XorReader::with_offset(&obfuscated[13..], key, 13)
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, plain[13..]);
    }

    #[test]
    fn xor_in_place_matches_bytewise_xor() {
        let key: XorKey = [0xde, 0xca, 0x6d, 0x01, 0x00, 0x56, 0x05, 0xe5];

        // Lengths and offsets around the 8-byte boundary
        for base in [0u64, 1, 5, 8, 13] {
            for len in [0usize, 1, 7, 8, 9, 16, 23, 1000, 1001] {
                let plain: Vec<u8> = (0..len).map(|i| i as u8).collect();

                let mut fast = plain.clone();
                xor_in_place_at(&mut fast, key, base);

                let bytewise: Vec<u8> = plain
                    .iter()
                    .enumerate()
                    .map(|(i, b)| b ^ key[(base as usize + i) % XOR_KEY_LEN])
                    .collect();

                assert_eq!(fast, bytewise, "base={} len={}", base, len);
            }
        }
    }

    #[test]
    fn zero_key_is_identity() {
        let data = [1u8, 2, 3, 4];
        let mut decoded = Vec::new();
        XorReader::new(&data[..], [0; XOR_KEY_LEN])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, data);
    }
}
