/*
 * Copyright (c) 2021 Works Applications Co., Ltd.
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

use std::iter::FusedIterator;

use crate::dic::lexicon_set::LexiconSet;
use crate::dic::read::u32_parser;
use crate::dic::read::word_info::WordInfoParser;
use crate::dic::subset::InfoSubset;
use crate::dic::word_id::WordId;
use crate::prelude::*;

/// Magic marker for block-compressed word_infos (MARISA converted dictionaries).
#[cfg(feature = "marisa-trie")]
const BLOCK_WI_MAGIC: u32 = 0x4D57_4942; // "MWIB"
#[cfg(feature = "marisa-trie")]
const VBYTE_WI_MAGIC: u32 = 0x4D57_5642; // "MWVB"

pub struct WordInfos<'a> {
    bytes: &'a [u8],
    offset: usize,
    _word_size: u32,
    has_synonym_group_ids: bool,
    #[cfg(feature = "marisa-trie")]
    block_compressed: Option<BlockCompressedInfo>,
}

/// Block-compressed word_infos.
///
/// Records are grouped into zstd-compressed blocks with per-record
/// intra-block offsets. A single-entry cache avoids redundant block
/// decompression for sequential access patterns.
#[cfg(feature = "marisa-trie")]
struct BlockCompressedInfo {
    record_offsets_start: usize,
    records_per_block: usize,
    num_blocks: usize,
    block_index_start: usize,
    compressed_data_start: usize,
    cache: std::sync::Mutex<BlockCache>,
}

#[cfg(feature = "marisa-trie")]
struct BlockCache {
    block_idx: usize,
    data: Vec<u8>,
    decoder: ruzstd::decoding::FrameDecoder,
}

impl<'a> WordInfos<'a> {
    pub fn new(
        bytes: &'a [u8],
        offset: usize,
        _word_size: u32,
        has_synonym_group_ids: bool,
    ) -> SudachiResult<WordInfos<'a>> {
        #[cfg(feature = "marisa-trie")]
        {
            let maybe_magic = if offset + 4 <= bytes.len() {
                u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
            } else {
                0
            };

            if maybe_magic == BLOCK_WI_MAGIC {
                let mut off = offset + 4;
                if off + 4 > bytes.len() {
                    return Err(SudachiError::InvalidRange(off, off + 4));
                }
                let num_words = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
                off += 4;
                if off + 2 > bytes.len() {
                    return Err(SudachiError::InvalidRange(off, off + 2));
                }
                let records_per_block =
                    u16::from_le_bytes(bytes[off..off + 2].try_into().unwrap()) as usize;
                off += 2;
                if off + 4 > bytes.len() {
                    return Err(SudachiError::InvalidRange(off, off + 4));
                }
                let num_blocks = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
                off += 4;
                if off + 4 > bytes.len() {
                    return Err(SudachiError::InvalidRange(off, off + 4));
                }
                let dict_size = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
                off += 4;
                let dict_end = off + dict_size;
                if dict_end > bytes.len() {
                    return Err(SudachiError::InvalidRange(off, dict_end));
                }
                let dict_bytes = &bytes[off..dict_end];
                off = dict_end;

                let mut decoder = ruzstd::decoding::FrameDecoder::new();
                if dict_size > 0 {
                    let dict = ruzstd::decoding::Dictionary::decode_dict(dict_bytes).map_err(|e| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("word_infos dict decode failed: {:?}", e),
                        )
                    })?;
                    decoder.add_dict(dict).map_err(|e| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("word_infos add_dict failed: {:?}", e),
                        )
                    })?;
                }

                let record_offsets_start = off;
                off += num_words * 4;
                let block_index_start = off;
                off += num_blocks * 8;
                let compressed_data_start = off;

                return Ok(WordInfos {
                    bytes,
                    offset,
                    _word_size: num_words as u32,
                    has_synonym_group_ids,
                    block_compressed: Some(BlockCompressedInfo {
                        record_offsets_start,
                        records_per_block,
                        num_blocks,
                        block_index_start,
                        compressed_data_start,
                        cache: std::sync::Mutex::new(BlockCache {
                            block_idx: usize::MAX,
                            data: Vec::new(),
                            decoder,
                        }),
                    }),
                });
            }

            if maybe_magic == VBYTE_WI_MAGIC {
                return Err(SudachiError::InvalidDictionaryGrammar.with_context(
                    "VByte-encoded word infos are no longer supported",
                ));
            }

            return Ok(WordInfos {
                bytes,
                offset,
                _word_size,
                has_synonym_group_ids,
                block_compressed: None,
            });
        }

        #[cfg(not(feature = "marisa-trie"))]
        Ok(WordInfos {
            bytes,
            offset,
            _word_size,
            has_synonym_group_ids,
        })
    }

    fn word_id_to_offset(&self, word_id: u32) -> SudachiResult<usize> {
        Ok(u32_parser(&self.bytes[self.offset + (4 * word_id as usize)..])?.1 as usize)
    }

    fn parse_word_info(&self, word_id: u32, subset: InfoSubset) -> SudachiResult<WordInfoData> {
        #[cfg(feature = "marisa-trie")]
        if let Some(ref bc) = self.block_compressed {
            return self.parse_word_info_block(bc, word_id, subset);
        }

        let index = self.word_id_to_offset(word_id)?;
        let parser = WordInfoParser::subset(subset);
        parser.parse(&self.bytes[index..])
    }

    /// Parse a word_info record from a block-compressed section.
    ///
    /// Finds the block containing `word_id`, decompresses it if needed,
    /// then parses the record at the stored intra-block offset.
    #[cfg(feature = "marisa-trie")]
    fn parse_word_info_block(
        &self,
        bc: &BlockCompressedInfo,
        word_id: u32,
        subset: InfoSubset,
    ) -> SudachiResult<WordInfoData> {
        let block_idx = word_id as usize / bc.records_per_block;
        if block_idx >= bc.num_blocks {
            return Err(SudachiError::InvalidRange(block_idx, bc.num_blocks));
        }

        let mut cache = bc.cache.lock().map_err(|_| {
            SudachiError::from(std::io::Error::new(
                std::io::ErrorKind::Other,
                "word_info cache lock poisoned",
            ))
        })?;

        if cache.block_idx != block_idx {
            let bi_off = bc.block_index_start + block_idx * 8;
            let block_offset =
                u32::from_le_bytes(self.bytes[bi_off..bi_off + 4].try_into().unwrap()) as usize;
            let block_size =
                u32::from_le_bytes(self.bytes[bi_off + 4..bi_off + 8].try_into().unwrap()) as usize;

            let compressed = &self.bytes[bc.compressed_data_start + block_offset
                ..bc.compressed_data_start + block_offset + block_size];

            cache.data = {
                use std::io::Read;
                let mut cursor = std::io::Cursor::new(compressed);
                cache.decoder.reset(&mut cursor).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("word_info block zstd init error: {:?}", e),
                    )
                })?;
                cache
                    .decoder
                    .decode_blocks(&mut cursor, ruzstd::decoding::BlockDecodingStrategy::All)
                    .map_err(|e| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            format!("word_info block zstd decode error: {:?}", e),
                        )
                    })?;
                let mut buf = Vec::new();
                cache.decoder.read_to_end(&mut buf).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("word_info block zstd collect error: {}", e),
                    )
                })?;
                buf
            };
            cache.block_idx = block_idx;
        }

        let ro_off = bc.record_offsets_start + word_id as usize * 4;
        if ro_off + 4 > self.bytes.len() {
            return Err(SudachiError::InvalidRange(ro_off, ro_off + 4));
        }
        let record_offset =
            u32::from_le_bytes(self.bytes[ro_off..ro_off + 4].try_into().unwrap()) as usize;
        if record_offset > cache.data.len() {
            return Err(SudachiError::InvalidRange(record_offset, cache.data.len()));
        }
        let parser = WordInfoParser::subset(subset);
        parser.parse(&cache.data[record_offset..])
    }

    pub fn get_word_info(&self, word_id: u32, mut subset: InfoSubset) -> SudachiResult<WordInfo> {
        if !self.has_synonym_group_ids {
            subset -= InfoSubset::SYNONYM_GROUP_ID;
        }

        let mut word_info = self.parse_word_info(word_id, subset)?;

        // consult dictionary form
        let dfwi = word_info.dictionary_form_word_id;
        if (dfwi >= 0) && (dfwi != word_id as i32) {
            let inner = self.parse_word_info(dfwi as u32, InfoSubset::SURFACE)?;
            word_info.dictionary_form = inner.surface;
        };

        Ok(word_info.into())
    }
}

/// Internal storage of the WordInfo.
/// It is not accessible by default, but a WordInfo can be created from it:
/// `let wi: WordInfo = data.into();`
///
/// String fields CAN be empty, in this case the value of the surface field should be used instead
#[derive(Clone, Debug, Default)]
pub struct WordInfoData {
    pub surface: String,
    pub head_word_length: u16,
    pub pos_id: u16,
    pub normalized_form: String,
    pub dictionary_form_word_id: i32,
    pub dictionary_form: String,
    pub reading_form: String,
    pub a_unit_split: Vec<WordId>,
    pub b_unit_split: Vec<WordId>,
    pub word_structure: Vec<WordId>,
    pub synonym_group_ids: Vec<u32>,
}

/// WordInfo API.
///
/// Internal data is not accessible by default, but can be extracted as
/// `let data: WordInfoData = info.into()`.
/// Note: this will consume WordInfo.
#[derive(Clone, Default)]
#[repr(transparent)]
pub struct WordInfo {
    data: WordInfoData,
}

impl WordInfo {
    pub fn surface(&self) -> &str {
        &self.data.surface
    }

    pub fn head_word_length(&self) -> usize {
        self.data.head_word_length as usize
    }

    pub fn pos_id(&self) -> u16 {
        self.data.pos_id
    }

    pub fn normalized_form(&self) -> &str {
        if self.data.normalized_form.is_empty() {
            self.surface()
        } else {
            &self.data.normalized_form
        }
    }

    pub fn dictionary_form_word_id(&self) -> i32 {
        self.data.dictionary_form_word_id
    }

    pub fn dictionary_form(&self) -> &str {
        if self.data.dictionary_form.is_empty() {
            self.surface()
        } else {
            &self.data.dictionary_form
        }
    }

    pub fn reading_form(&self) -> &str {
        if self.data.reading_form.is_empty() {
            self.surface()
        } else {
            &self.data.reading_form
        }
    }

    pub fn a_unit_split(&self) -> &[WordId] {
        &self.data.a_unit_split
    }

    pub fn b_unit_split(&self) -> &[WordId] {
        &self.data.b_unit_split
    }

    pub fn word_structure(&self) -> &[WordId] {
        &self.data.word_structure
    }

    pub fn synonym_group_ids(&self) -> &[u32] {
        &self.data.synonym_group_ids
    }

    pub fn borrow_data(&self) -> &WordInfoData {
        &self.data
    }
}

impl From<WordInfoData> for WordInfo {
    fn from(data: WordInfoData) -> Self {
        WordInfo { data }
    }
}

impl From<WordInfo> for WordInfoData {
    fn from(info: WordInfo) -> Self {
        info.data
    }
}

struct SplitIter<'a> {
    index: usize,
    split: &'a [WordId],
    lexicon: &'a LexiconSet<'a>,
}

impl Iterator for SplitIter<'_> {
    type Item = SudachiResult<WordInfo>;

    fn next(&mut self) -> Option<Self::Item> {
        let idx = self.index;
        if idx >= self.split.len() {
            None
        } else {
            self.index += 1;
            Some(self.lexicon.get_word_info(self.split[idx]))
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let rem = self.split.len() - self.index;
        (rem, Some(rem))
    }
}

impl FusedIterator for SplitIter<'_> {}

#[cfg(all(test, feature = "marisa-trie"))]
mod tests {
    use super::WordInfos;

    #[test]
    fn rejects_vbyte_word_infos() {
        let bytes = 0x4D57_5642u32.to_le_bytes();
        let err = WordInfos::new(&bytes, 0, 0, false).err().unwrap();
        assert!(err
            .to_string()
            .contains("VByte-encoded word infos are no longer supported"));
    }
}
