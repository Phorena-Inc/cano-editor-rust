//! Block byte scans for the hot paths: the row table, search and the
//! highlighter.
//!
//! Each scan works a 64-byte block at a time: one compare turns a block into
//! a 64-bit mask of the bytes it wants, and the caller then visits only the
//! set bits.  A byte loop instead pays one compare and one hard-to-predict
//! branch per byte, and the release profile (`opt-level = "z"`) never
//! vectorizes it.

pub(crate) const BLOCK: usize = 64;

/// Bit `i` of the result is set when `block[i] == byte`.
#[cfg(all(target_arch = "x86_64", target_feature = "sse2"))]
pub(crate) fn byte_mask(block: &[u8; BLOCK], byte: u8) -> u64 {
    use std::arch::x86_64::{_mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi8};

    let mut mask = 0;
    // SAFETY: the `cfg` above admits this function only where SSE2 is
    // enabled at compile time (it is part of the x86_64 baseline), so the
    // intrinsics cannot meet a CPU without it.  Each `chunk` is a
    // `&[u8; 16]`: all 16 bytes are initialized and readable for the
    // borrow, `loadu` has no alignment requirement, and the read stays
    // inside the array.
    unsafe {
        let wanted = _mm_set1_epi8(byte as i8);
        for (lane, chunk) in block.as_chunks::<16>().0.iter().enumerate() {
            let bytes = _mm_loadu_si128(chunk.as_ptr().cast());
            // `movemask` fills only the low 16 bits, one per byte lane.
            let hits = _mm_movemask_epi8(_mm_cmpeq_epi8(bytes, wanted)) as u16;
            mask |= u64::from(hits) << (16 * lane);
        }
    }
    mask
}

#[cfg(not(all(target_arch = "x86_64", target_feature = "sse2")))]
pub(crate) fn byte_mask(block: &[u8; BLOCK], byte: u8) -> u64 {
    swar_mask(block, byte)
}

/// The portable form of [`byte_mask`]: eight bytes per `u64`, with no
/// `unsafe`.  Tests run it on every target against a byte loop.
#[cfg(any(test, not(all(target_arch = "x86_64", target_feature = "sse2"))))]
fn swar_mask(block: &[u8; BLOCK], byte: u8) -> u64 {
    const LOW_SEVEN: u64 = 0x7f7f_7f7f_7f7f_7f7f;
    const HIGH: u64 = 0x8080_8080_8080_8080;
    const ONES: u64 = 0x0101_0101_0101_0101;
    // Moves the high bit of byte `i` to bit `56 + i`.  Each product term
    // lands on a distinct bit, so no carry corrupts the top byte.
    const GATHER: u64 = 0x0102_0408_1020_4080;

    let mut mask = 0;
    for (lane, word) in block.as_chunks::<8>().0.iter().enumerate() {
        let x = u64::from_le_bytes(*word) ^ (ONES * u64::from(byte));
        // Exact zero-byte test: adding 0x7f to the low seven bits cannot
        // carry into the next byte, so the high bit stays clear only for
        // a byte of `x` that is zero, which is a match in `word`.
        let zero = !(((x & LOW_SEVEN) + LOW_SEVEN) | x) & HIGH;
        let packed = (zero >> 7).wrapping_mul(GATHER) >> 56;
        mask |= packed << (8 * lane);
    }
    mask
}

/// The first index at or after `from` holding `byte`.
pub(crate) fn find_byte(data: &[u8], byte: u8, from: usize) -> Option<usize> {
    let mut at = from.min(data.len());
    while let Some(block) = data[at..].first_chunk::<BLOCK>() {
        let mask = byte_mask(block, byte);
        if mask != 0 {
            return Some(at + mask.trailing_zeros() as usize);
        }
        at += BLOCK;
    }
    data[at..]
        .iter()
        .position(|candidate| *candidate == byte)
        .map(|offset| at + offset)
}

/// The first start `s >= from` with `data[s..]` beginning with `needle` and
/// `accept(s)` true.
///
/// Candidates are filtered on the needle's first and last bytes together,
/// which rejects nearly every false start before a full compare runs.
pub(crate) fn find(
    data: &[u8],
    needle: &[u8],
    from: usize,
    accept: impl Fn(usize) -> bool,
) -> Option<usize> {
    let (&first, &last) = (needle.first()?, needle.last()?);
    let last_start = data.len().checked_sub(needle.len())?;
    let gap = needle.len() - 1;
    let matched = |start: usize| data[start..].starts_with(needle) && accept(start);
    let mut at = from;
    // A block of starts `at..at + 64` also reads the last bytes at
    // `at + gap..at + gap + 64`; both windows must lie inside `data`.
    while at + BLOCK <= last_start + 1 {
        let (Some(heads), Some(tails)) = (
            data[at..].first_chunk::<BLOCK>(),
            data[at + gap..].first_chunk::<BLOCK>(),
        ) else {
            break;
        };
        let mut mask = byte_mask(heads, first) & byte_mask(tails, last);
        while mask != 0 {
            let start = at + mask.trailing_zeros() as usize;
            if matched(start) {
                return Some(start);
            }
            mask &= mask - 1;
        }
        at += BLOCK;
    }
    (at..=last_start).find(|start| matched(*start))
}

/// The last start `s < before` with `data[s..]` beginning with `needle` and
/// `accept(s)` true.  The mirror image of [`find`].
pub(crate) fn rfind(
    data: &[u8],
    needle: &[u8],
    before: usize,
    accept: impl Fn(usize) -> bool,
) -> Option<usize> {
    let (&first, &last) = (needle.first()?, needle.last()?);
    let last_start = data.len().checked_sub(needle.len())?;
    let gap = needle.len() - 1;
    let matched = |start: usize| data[start..].starts_with(needle) && accept(start);
    // Starts below `limit` remain to be tried.
    let mut limit = before.min(last_start + 1);
    while limit >= BLOCK {
        let at = limit - BLOCK;
        let (Some(heads), Some(tails)) = (
            data[at..].first_chunk::<BLOCK>(),
            data[at + gap..].first_chunk::<BLOCK>(),
        ) else {
            break;
        };
        let mut mask = byte_mask(heads, first) & byte_mask(tails, last);
        while mask != 0 {
            let bit = BLOCK - 1 - mask.leading_zeros() as usize;
            if matched(at + bit) {
                return Some(at + bit);
            }
            mask &= !(1 << bit);
        }
        limit = at;
    }
    (0..limit).rev().find(|start| matched(*start))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bytes from an LCG over a small alphabet, so short needles recur, plus
    /// the near misses 0x0b and 0x8a of the newline: one bit apart, and 0x8a
    /// has the high bit the SWAR test depends on.
    fn noise(len: usize, seed: &mut u64) -> Vec<u8> {
        (0..len)
            .map(|_| {
                *seed = seed
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                match (*seed >> 33) % 7 {
                    0 => b'\n',
                    1 => 0x0b,
                    2 => 0x8a,
                    3 => 0xff,
                    4 => b'a',
                    5 => b'b',
                    _ => (*seed >> 41) as u8,
                }
            })
            .collect()
    }

    #[test]
    fn swar_and_target_masks_agree_with_a_byte_loop() {
        let mut seed = 7;
        for byte in [b'\n', b'a', 0x00, 0x80, 0xff] {
            for _ in 0..200 {
                let data = noise(BLOCK, &mut seed);
                let block: &[u8; BLOCK] = data.as_slice().try_into().unwrap();
                let expected = data
                    .iter()
                    .enumerate()
                    .fold(0u64, |mask, (i, &b)| mask | (u64::from(b == byte) << i));
                assert_eq!(swar_mask(block, byte), expected);
                assert_eq!(byte_mask(block, byte), expected);
            }
        }
    }

    #[test]
    fn find_byte_matches_a_byte_loop_from_every_offset() {
        let mut seed = 3;
        for len in [0, 1, 63, 64, 65, 200] {
            let data = noise(len, &mut seed);
            for from in 0..=len + 1 {
                let expected = data
                    .get(from..)
                    .and_then(|rest| rest.iter().position(|b| *b == b'\n'))
                    .map(|offset| from + offset);
                assert_eq!(find_byte(&data, b'\n', from), expected, "{len} {from}");
            }
        }
    }

    #[test]
    fn find_and_rfind_match_a_byte_loop_across_block_edges() {
        let mut seed = 11;
        let needles: [&[u8]; 5] = [b"a", b"ab", b"ba\n", b"abba", b"aaaaaaaa"];
        for len in [0, 3, 63, 64, 65, 70, 130, 300] {
            let data = noise(len, &mut seed);
            // An acceptance test that rejects some starts, as whole-word
            // matching does.
            let accept = |at: usize| at % 3 != 1;
            for needle in needles {
                let starts = |range: std::ops::Range<usize>| {
                    range
                        .filter(|s| data[*s..].starts_with(needle) && accept(*s))
                        .collect::<Vec<_>>()
                };
                for from in 0..=len + 1 {
                    let all = starts(from.min(len)..len);
                    assert_eq!(
                        find(&data, needle, from, accept),
                        all.first().copied(),
                        "find {needle:?} len {len} from {from}"
                    );
                    let all = starts(0..from.min(len));
                    assert_eq!(
                        rfind(&data, needle, from, accept),
                        all.last().copied(),
                        "rfind {needle:?} len {len} before {from}"
                    );
                }
            }
        }
    }
}
