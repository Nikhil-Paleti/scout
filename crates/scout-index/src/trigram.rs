//! Trigram extraction. Trigrams are three ASCII-lowercased bytes packed into
//! the low 24 bits of a `u32`. Trigrams that contain a newline are never
//! indexed, because matches are line oriented.

pub const NUM_TRIGRAMS: usize = 1 << 24;

#[inline]
pub fn pack(a: u8, b: u8, c: u8) -> u32 {
    ((a as u32) << 16) | ((b as u32) << 8) | (c as u32)
}

#[inline]
pub fn unpack(t: u32) -> [u8; 3] {
    [(t >> 16) as u8, (t >> 8) as u8, t as u8]
}

/// Reusable per-thread scratch space: a 2 MiB bitset over all trigrams.
pub struct Extractor {
    seen: Vec<u64>,
}

impl Default for Extractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor {
    pub fn new() -> Self {
        Extractor { seen: vec![0u64; NUM_TRIGRAMS / 64] }
    }

    /// Appends the distinct trigrams of `data` to `out`, sorted ascending.
    pub fn extract(&mut self, data: &[u8], out: &mut Vec<u32>) {
        let start = out.len();
        if data.len() >= 3 {
            let mut a = data[0].to_ascii_lowercase();
            let mut b = data[1].to_ascii_lowercase();
            for &raw in &data[2..] {
                let c = raw.to_ascii_lowercase();
                if a != b'\n' && b != b'\n' && c != b'\n' {
                    let t = pack(a, b, c) as usize;
                    let (w, bit) = (t >> 6, 1u64 << (t & 63));
                    if self.seen[w] & bit == 0 {
                        self.seen[w] |= bit;
                        out.push(t as u32);
                    }
                }
                a = b;
                b = c;
            }
        }
        for &t in &out[start..] {
            self.seen[(t as usize) >> 6] = 0;
        }
        out[start..].sort_unstable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_skips_newlines() {
        let mut ex = Extractor::new();
        let mut out = Vec::new();
        ex.extract(b"AbC\nabc", &mut out);
        assert_eq!(out, vec![pack(b'a', b'b', b'c')]);
        // Scratch is cleared between calls.
        out.clear();
        ex.extract(b"xyz", &mut out);
        assert_eq!(out, vec![pack(b'x', b'y', b'z')]);
    }
}
