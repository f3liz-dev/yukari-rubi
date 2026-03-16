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

//! Pre-built reading-keyed MARISA trie for KKC (Kana-Kanji Conversion).
//!
//! Instead of building a reverse reading index (`BTreeMap<String, Vec<WordId>>`)
//! at runtime by iterating every word in the dictionary (逆引き), this module
//! loads a reading-keyed trie that was built at dictionary conversion time.
//!
//! The reading trie supports efficient common-prefix search: given katakana
//! input bytes, it yields all readings that are prefixes of the input, along
//! with the word IDs that have each reading. This enables O(n) lattice
//! construction instead of O(n²) BTreeMap lookups.

use super::marisa_trie::MarisaTrie;
use super::word_id_table::WordIdTable;

use crate::error::SudachiResult;

/// Magic marker for the reading trie section in the dictionary binary.
pub const READING_TRIE_MAGIC: u32 = 0x5254_5249; // "RTRI"

/// A pre-built reading-keyed trie for KKC lookup.
///
/// Maps katakana reading bytes → word IDs via a MARISA trie + word_id_table.
pub struct ReadingTrie<'a> {
    trie: MarisaTrie,
    word_id_table: WordIdTable<'a>,
}

/// A single match from a reading trie common-prefix search.
#[derive(Debug)]
pub struct ReadingMatch {
    /// Byte offset in the input where this reading match ends.
    pub end: usize,
    /// Word IDs that have this reading (raw u32, without dic_id prefix).
    pub word_ids: Vec<u32>,
}

impl<'a> ReadingTrie<'a> {
    /// Deserialize a reading trie from the dictionary binary at `offset`.
    ///
    /// Expects the data to start with [`READING_TRIE_MAGIC`].
    /// Returns the ReadingTrie and the number of bytes consumed.
    pub fn from_bytes(buf: &'a [u8], offset: usize) -> SudachiResult<(ReadingTrie<'a>, usize)> {
        let mut pos = offset;

        // Read and verify magic
        if pos + 4 > buf.len() {
            return Err(crate::error::SudachiError::InvalidRange(pos, pos + 4));
        }
        let magic = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
        if magic != READING_TRIE_MAGIC {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Expected RTRI magic 0x{:08X}, got 0x{:08X}",
                    READING_TRIE_MAGIC, magic
                ),
            )
            .into());
        }
        pos += 4;

        // Read MarisaTrie (trie + id_to_offset)
        let (trie, trie_consumed) = MarisaTrie::from_bytes(buf, pos)?;
        pos += trie_consumed;

        // Read word_id_table
        if pos + 4 > buf.len() {
            return Err(crate::error::SudachiError::InvalidRange(pos, pos + 4));
        }
        let table_size = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
        let word_id_table = WordIdTable::new(buf, table_size, pos + 4);
        pos += 4 + table_size as usize;

        let consumed = pos - offset;
        Ok((ReadingTrie { trie, word_id_table }, consumed))
    }

    /// Common-prefix search over katakana reading bytes.
    ///
    /// Given `input` bytes starting at `offset`, yields all readings that are
    /// prefixes of `input[offset..]`, along with the raw word IDs for each.
    ///
    /// Each returned [`ReadingMatch`] contains:
    /// - `end`: byte offset in `input` where the match ends
    /// - `word_ids`: raw u32 word IDs (without dic_id prefix)
    pub fn common_prefix_search(&self, input: &[u8], offset: usize) -> Vec<ReadingMatch> {
        let mut results = Vec::new();
        for entry in self.trie.common_prefix_iterator(input, offset) {
            let word_ids: Vec<u32> = self
                .word_id_table
                .entries(entry.value as usize)
                .collect();
            results.push(ReadingMatch {
                end: entry.end,
                word_ids,
            });
        }
        results
    }

    /// Look up all word IDs whose reading exactly matches `katakana`.
    ///
    /// Returns raw u32 word IDs (without dic_id prefix).
    pub fn lookup_exact(&self, katakana: &str) -> Vec<u32> {
        let katakana_bytes = katakana.as_bytes();
        // Common-prefix search, then filter for exact length match
        for entry in self.trie.common_prefix_iterator(katakana_bytes, 0) {
            if entry.end == katakana_bytes.len() {
                return self
                    .word_id_table
                    .entries(entry.value as usize)
                    .collect();
            }
        }
        Vec::new()
    }

    /// Look up all word IDs whose reading starts with `katakana_prefix`.
    ///
    /// Uses the MARISA trie's predictive search to find all keys with the
    /// given prefix. Returns (reading, raw_word_ids) pairs.
    pub fn lookup_prefix(&self, katakana_prefix: &str) -> Vec<(String, Vec<u32>)> {
        self.trie.predictive_search_entries(katakana_prefix, &self.word_id_table)
    }
}
