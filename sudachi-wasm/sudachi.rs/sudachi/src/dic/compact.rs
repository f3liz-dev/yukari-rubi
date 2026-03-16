/*
 * Copyright (c) 2021-2024 Works Applications Co., Ltd.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Compact encoding primitives: VByte, ZigZag, and bit-packing.
//!
//! Used by the connection matrix (bit-packed blocks) and word_infos
//! (VByte-encoded records) for space-efficient dictionary storage
//! that can be decoded without heavy decompression libraries.

/// Encode a u32 as VByte (LEB128-style).
///
/// Each byte stores 7 data bits; bit 7 = 1 means more bytes follow.
#[inline]
pub fn encode_vbyte(mut value: u32, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Decode a VByte-encoded u32 from `data`.
///
/// Returns `(value, bytes_consumed)`.
#[inline]
pub fn decode_vbyte(data: &[u8]) -> (u32, usize) {
    let mut result: u32 = 0;
    let mut shift = 0u32;
    for (i, &byte) in data.iter().enumerate() {
        result |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return (result, i + 1);
        }
        shift += 7;
    }
    (result, data.len())
}

/// ZigZag encode: maps signed integers to unsigned so that small-magnitude
/// values (positive or negative) produce small unsigned values.
///
/// 0 → 0, -1 → 1, 1 → 2, -2 → 3, 2 → 4, ...
#[inline]
pub fn encode_zigzag(value: i32) -> u32 {
    ((value << 1) ^ (value >> 31)) as u32
}

/// ZigZag decode: inverse of `encode_zigzag`.
#[inline]
pub fn decode_zigzag(value: u32) -> i32 {
    ((value >> 1) as i32) ^ -((value & 1) as i32)
}

/// Extract `bit_width` bits from a packed buffer at the given bit offset.
///
/// Supports bit_width 0..=16. Returns the extracted value as u16.
#[inline]
pub fn extract_bits(data: &[u8], bit_offset: usize, bit_width: u8) -> u16 {
    if bit_width == 0 {
        return 0;
    }
    let byte_pos = bit_offset / 8;
    let bit_pos = (bit_offset % 8) as u32;

    // Read up to 4 bytes to cover any bit span (bit_width ≤ 16, bit_pos ≤ 7 → max 23 bits)
    let mut val: u32 = data[byte_pos] as u32;
    if bit_pos + bit_width as u32 > 8 {
        val |= (data[byte_pos + 1] as u32) << 8;
    }
    if bit_pos + bit_width as u32 > 16 {
        val |= (data[byte_pos + 2] as u32) << 16;
    }
    ((val >> bit_pos) & ((1u32 << bit_width) - 1)) as u16
}

/// Pack `bit_width` bits of `value` into the buffer at the given bit offset.
///
/// Assumes the target bits in `data` are already zeroed.
#[inline]
pub fn pack_bits(data: &mut [u8], bit_offset: usize, value: u16, bit_width: u8) {
    if bit_width == 0 {
        return;
    }
    let byte_pos = bit_offset / 8;
    let bit_pos = (bit_offset % 8) as u32;
    let val = (value as u32) << bit_pos;

    data[byte_pos] |= val as u8;
    if bit_pos + bit_width as u32 > 8 {
        data[byte_pos + 1] |= (val >> 8) as u8;
    }
    if bit_pos + bit_width as u32 > 16 {
        data[byte_pos + 2] |= (val >> 16) as u8;
    }
}

/// Compute the minimum number of bits needed to represent values in range [0, max_val].
#[inline]
pub fn bits_needed(max_val: u16) -> u8 {
    if max_val == 0 {
        return 0;
    }
    16 - max_val.leading_zeros() as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vbyte_roundtrip() {
        let cases = [0u32, 1, 127, 128, 16383, 16384, 0x0FFF_FFFF, u32::MAX];
        for &val in &cases {
            let mut buf = Vec::new();
            encode_vbyte(val, &mut buf);
            let (decoded, consumed) = decode_vbyte(&buf);
            assert_eq!(decoded, val, "VByte roundtrip failed for {}", val);
            assert_eq!(consumed, buf.len());
        }
    }

    #[test]
    fn test_zigzag_roundtrip() {
        let cases = [0i32, -1, 1, -2, 2, i32::MIN, i32::MAX, -12345, 12345];
        for &val in &cases {
            let encoded = encode_zigzag(val);
            let decoded = decode_zigzag(encoded);
            assert_eq!(decoded, val, "ZigZag roundtrip failed for {}", val);
        }
    }

    #[test]
    fn test_bits_roundtrip() {
        let mut data = vec![0u8; 16];
        pack_bits(&mut data, 0, 0b10110, 5);
        assert_eq!(extract_bits(&data, 0, 5), 0b10110);

        pack_bits(&mut data, 5, 0b111, 3);
        assert_eq!(extract_bits(&data, 5, 3), 0b111);

        // Cross-byte boundary
        let mut data2 = vec![0u8; 16];
        pack_bits(&mut data2, 6, 0xABCD, 16);
        assert_eq!(extract_bits(&data2, 6, 16), 0xABCD);
    }

    #[test]
    fn test_bits_needed() {
        assert_eq!(bits_needed(0), 0);
        assert_eq!(bits_needed(1), 1);
        assert_eq!(bits_needed(2), 2);
        assert_eq!(bits_needed(3), 2);
        assert_eq!(bits_needed(255), 8);
        assert_eq!(bits_needed(256), 9);
        assert_eq!(bits_needed(u16::MAX), 16);
    }
}
