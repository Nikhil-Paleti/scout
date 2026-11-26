//! LEB128-style unsigned varints, used for delta-encoded posting lists.

#[inline]
pub fn encode(mut v: u32, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Decodes one varint from `buf[*pos..]`, advancing `pos`.
#[inline]
pub fn decode(buf: &[u8], pos: &mut usize) -> u32 {
    let mut v: u32 = 0;
    let mut shift = 0;
    loop {
        let b = buf[*pos];
        *pos += 1;
        v |= ((b & 0x7f) as u32) << shift;
        if b < 0x80 {
            return v;
        }
        shift += 7;
    }
}

/// Encodes a strictly increasing list of ids as deltas.
pub fn encode_postings(ids: &[u32], out: &mut Vec<u8>) {
    let mut prev: u32 = 0;
    for (i, &id) in ids.iter().enumerate() {
        let d = if i == 0 { id } else { id - prev };
        encode(d, out);
        prev = id;
    }
}

/// Decodes `count` delta-encoded ids from `buf`.
pub fn decode_postings(buf: &[u8], count: usize, out: &mut Vec<u32>) {
    out.reserve(count);
    let mut pos = 0;
    let mut prev: u32 = 0;
    for i in 0..count {
        let d = decode(buf, &mut pos);
        let id = if i == 0 { d } else { prev + d };
        out.push(id);
        prev = id;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let ids = vec![0, 1, 2, 127, 128, 300, 16_384, 1 << 28, u32::MAX];
        let mut buf = Vec::new();
        encode_postings(&ids, &mut buf);
        let mut out = Vec::new();
        decode_postings(&buf, ids.len(), &mut out);
        assert_eq!(ids, out);
    }
}
