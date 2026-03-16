/*
 *  Copyright (c) 2021 Works Applications Co., Ltd.
 *
 *  Licensed under the Apache License, Version 2.0 (the "License");
 *  you may not use this file except in compliance with the License.
 *  You may obtain a copy of the License at
 *
 *      http://www.apache.org/licenses/LICENSE-2.0
 *
 *   Unless required by applicable law or agreed to in writing, software
 *  distributed under the License is distributed on an "AS IS" BASIS,
 *  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *  See the License for the specific language governing permissions and
 *  limitations under the License.
 */

use crate::error::{SudachiError, SudachiResult};

#[cfg(not(feature = "marisa-trie"))]
use crate::util::cow_array::CowArray;

/// The flat (uncompressed) connection matrix, used when `marisa-trie` is NOT enabled.
#[cfg(not(feature = "marisa-trie"))]
pub struct ConnectionMatrix<'a> {
    data: CowArray<'a, i16>,
    num_left: usize,
    num_right: usize,
}

#[cfg(not(feature = "marisa-trie"))]
impl<'a> ConnectionMatrix<'a> {
    pub fn from_offset_size(
        data: &'a [u8],
        offset: usize,
        num_left: usize,
        num_right: usize,
    ) -> SudachiResult<ConnectionMatrix<'a>> {
        let size = num_left * num_right;

        let end = offset + size;
        if end > data.len() {
            return Err(SudachiError::InvalidDictionaryGrammar.with_context("connection matrix"));
        }

        Ok(ConnectionMatrix {
            data: CowArray::from_bytes(data, offset, size),
            num_left,
            num_right,
        })
    }

    #[inline(always)]
    fn index(&self, left: u16, right: u16) -> usize {
        let uleft = left as usize;
        let uright = right as usize;
        debug_assert!(uleft < self.num_left);
        debug_assert!(uright < self.num_right);
        let index = uright * self.num_left + uleft;
        debug_assert!(index < self.data.len());
        index
    }

    #[inline(always)]
    pub fn cost(&self, left: u16, right: u16) -> i16 {
        let index = self.index(left, right);
        self.data.get(index).copied().unwrap_or(0)
    }

    pub fn update(&mut self, left: u16, right: u16, value: i16) {
        let index = self.index(left, right);
        self.data.set(index, value);
    }

    pub fn num_left(&self) -> usize {
        self.num_left
    }

    pub fn num_right(&self) -> usize {
        self.num_right
    }
}

#[cfg(feature = "marisa-trie")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompressionKind {
    Uncompressed,
    Zstd,
}

/// Connection matrix for the `marisa-trie` feature.
///
/// Supports two layouts:
/// - flat uncompressed matrices from `DictBuilder`
/// - zstd-compressed block matrices (`MCZB`) from `dic_converter`
#[cfg(feature = "marisa-trie")]
pub struct ConnectionMatrix<'a> {
    buf: &'a [u8],
    num_left: usize,
    num_right: usize,
    num_col_blocks: usize,
    index_start: usize,
    data_start: usize,
    kind: CompressionKind,
    cache: std::sync::Mutex<StripeCache>,
}

#[cfg(feature = "marisa-trie")]
struct StripeCache {
    row_block: usize,
    data: Vec<i16>,
    decoder: ruzstd::decoding::FrameDecoder,
}

#[cfg(feature = "marisa-trie")]
impl<'a> ConnectionMatrix<'a> {
    /// Block size for compressed matrices (256×256 cells per block).
    pub const BLOCK_SIZE: usize = 256;

    /// zstd-compressed connection matrix magic: "MCZB"
    pub const COMPRESSED_MAGIC: u32 = 0x4D43_5A42;

    /// Read the flat (uncompressed) connection matrix — used when loading
    /// dictionaries that have NOT been converted (e.g., dictionaries built
    /// from source via `DictBuilder`).
    pub fn from_offset_size(
        buf: &'a [u8],
        offset: usize,
        num_left: usize,
        num_right: usize,
    ) -> SudachiResult<ConnectionMatrix<'a>> {
        let size = num_left * num_right;
        let byte_size = size * 2;
        if offset + byte_size > buf.len() {
            return Err(SudachiError::InvalidDictionaryGrammar.with_context("connection matrix"));
        }

        let mut data = vec![0i16; size];
        for (i, slot) in data.iter_mut().enumerate() {
            let pos = offset + i * 2;
            *slot = i16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap());
        }

        Ok(ConnectionMatrix {
            buf,
            num_left,
            num_right,
            num_col_blocks: 0,
            index_start: 0,
            data_start: 0,
            kind: CompressionKind::Uncompressed,
            cache: std::sync::Mutex::new(StripeCache {
                row_block: 0,
                data,
                decoder: ruzstd::decoding::FrameDecoder::new(),
            }),
        })
    }

    /// Read a zstd-compressed connection matrix from the dictionary.
    pub fn from_compressed(
        buf: &'a [u8],
        offset: usize,
    ) -> SudachiResult<(ConnectionMatrix<'a>, usize)> {
        let mut pos = offset;

        if pos + 4 > buf.len() {
            return Err(
                SudachiError::InvalidDictionaryGrammar.with_context("compressed conn header"),
            );
        }
        let num_left = u16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap()) as usize;
        let num_right = u16::from_le_bytes(buf[pos + 2..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        let num_blocks = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        if pos + 4 > buf.len() {
            return Err(
                SudachiError::InvalidDictionaryGrammar.with_context("compressed conn dict size"),
            );
        }
        let dict_size = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        if pos + dict_size > buf.len() {
            return Err(
                SudachiError::InvalidDictionaryGrammar.with_context("compressed conn dictionary"),
            );
        }
        let dict_bytes = &buf[pos..pos + dict_size];
        pos += dict_size;

        let index_start = pos;
        let index_size = num_blocks * 8;
        if pos + index_size > buf.len() {
            return Err(
                SudachiError::InvalidDictionaryGrammar.with_context("compressed conn index"),
            );
        }
        pos += index_size;
        let data_start = pos;

        let mut max_data_end = data_start;
        for i in 0..num_blocks {
            let idx_off = index_start + i * 8;
            let blk_offset =
                u32::from_le_bytes(buf[idx_off..idx_off + 4].try_into().unwrap()) as usize;
            let blk_size =
                u32::from_le_bytes(buf[idx_off + 4..idx_off + 8].try_into().unwrap()) as usize;
            let abs_end = data_start + blk_offset + blk_size;
            if abs_end > max_data_end {
                max_data_end = abs_end;
            }
        }

        let num_col_blocks = (num_left + Self::BLOCK_SIZE - 1) / Self::BLOCK_SIZE;

        let mut decoder = ruzstd::decoding::FrameDecoder::new();
        if dict_size > 0 {
            let dict = ruzstd::decoding::Dictionary::decode_dict(dict_bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("connection matrix dict decode failed: {:?}", e),
                )
            })?;
            decoder.add_dict(dict).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("connection matrix add_dict failed: {:?}", e),
                )
            })?;
        }

        let cache = StripeCache {
            row_block: usize::MAX,
            data: vec![0i16; Self::BLOCK_SIZE * num_left],
            decoder,
        };

        let consumed = max_data_end - offset;
        Ok((
            ConnectionMatrix {
                buf,
                num_left,
                num_right,
                num_col_blocks,
                index_start,
                data_start,
                kind: CompressionKind::Zstd,
                cache: std::sync::Mutex::new(cache),
            },
            consumed,
        ))
    }

    fn load_row_stripe(
        cache: &mut StripeCache,
        buf: &[u8],
        row_block: usize,
        num_left: usize,
        num_right: usize,
        num_col_blocks: usize,
        index_start: usize,
        data_start: usize,
    ) {
        let bs = Self::BLOCK_SIZE;
        let row_start = row_block * bs;
        let row_end = std::cmp::min(row_start + bs, num_right);
        let actual_rows = row_end - row_start;

        for value in &mut cache.data[..actual_rows * num_left] {
            *value = 0;
        }

        for cb in 0..num_col_blocks {
            let blk_idx = row_block * num_col_blocks + cb;
            let idx_off = index_start + blk_idx * 8;
            let blk_offset = u32::from_le_bytes(buf[idx_off..idx_off + 4].try_into().unwrap())
                as usize;
            let blk_size =
                u32::from_le_bytes(buf[idx_off + 4..idx_off + 8].try_into().unwrap()) as usize;

            let abs_offset = data_start + blk_offset;
            let compressed = &buf[abs_offset..abs_offset + blk_size];

            let decompressed = {
                use std::io::Read;
                let mut cursor = std::io::Cursor::new(compressed);
                cache.decoder.reset(&mut cursor).unwrap_or_else(|e| {
                    panic!("connection matrix block {} zstd reset failed: {:?}", blk_idx, e);
                });
                cache
                    .decoder
                    .decode_blocks(&mut cursor, ruzstd::decoding::BlockDecodingStrategy::All)
                    .unwrap_or_else(|e| {
                        panic!("connection matrix block {} zstd decode failed: {:?}", blk_idx, e);
                    });
                let mut out = Vec::new();
                cache.decoder.read_to_end(&mut out).unwrap_or_else(|e| {
                    panic!("connection matrix block {} zstd collect failed: {}", blk_idx, e);
                });
                out
            };

            let col_start = cb * bs;
            let col_end = std::cmp::min(col_start + bs, num_left);
            let mut src = 0;
            for local_r in 0..actual_rows {
                for c in col_start..col_end {
                    if src + 2 <= decompressed.len() {
                        cache.data[local_r * num_left + c] =
                            i16::from_le_bytes(decompressed[src..src + 2].try_into().unwrap());
                    }
                    src += 2;
                }
            }
        }

        cache.row_block = row_block;
    }

    #[inline(always)]
    pub fn cost(&self, left: u16, right: u16) -> i16 {
        let uleft = left as usize;
        let uright = right as usize;
        debug_assert!(uleft < self.num_left);
        debug_assert!(uright < self.num_right);

        match self.kind {
            CompressionKind::Uncompressed => {
                let index = uright * self.num_left + uleft;
                let cache = self.cache.lock().unwrap();
                cache.data.get(index).copied().unwrap_or(0)
            }
            CompressionKind::Zstd => {
                let row_block = uright / Self::BLOCK_SIZE;
                let local_row = uright % Self::BLOCK_SIZE;

                let mut cache = self.cache.lock().unwrap();
                if cache.row_block != row_block {
                    Self::load_row_stripe(
                        &mut cache,
                        self.buf,
                        row_block,
                        self.num_left,
                        self.num_right,
                        self.num_col_blocks,
                        self.index_start,
                        self.data_start,
                    );
                }

                let index = local_row * self.num_left + uleft;
                cache.data.get(index).copied().unwrap_or(0)
            }
        }
    }

    pub fn update(&mut self, left: u16, right: u16, value: i16) {
        match self.kind {
            CompressionKind::Uncompressed => {
                let index = right as usize * self.num_left + left as usize;
                let mut cache = self.cache.lock().unwrap();
                if index < cache.data.len() {
                    cache.data[index] = value;
                }
            }
            CompressionKind::Zstd => {
                let uleft = left as usize;
                let uright = right as usize;
                let row_block = uright / Self::BLOCK_SIZE;
                let local_row = uright % Self::BLOCK_SIZE;

                let mut cache = self.cache.lock().unwrap();
                if cache.row_block != row_block {
                    Self::load_row_stripe(
                        &mut cache,
                        self.buf,
                        row_block,
                        self.num_left,
                        self.num_right,
                        self.num_col_blocks,
                        self.index_start,
                        self.data_start,
                    );
                }

                let index = local_row * self.num_left + uleft;
                if index < cache.data.len() {
                    cache.data[index] = value;
                }
            }
        }
    }

    pub fn num_left(&self) -> usize {
        self.num_left
    }

    pub fn num_right(&self) -> usize {
        self.num_right
    }
}
