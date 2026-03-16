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

//! MARISA trie backend for the Sudachi lexicon.
//!
//! When the `marisa-trie` feature is enabled, this module provides a
//! [`MarisaTrie`] wrapper that implements the same common-prefix-search
//! interface as the YADA-based [`super::trie::Trie`], but backed by the
//! space-efficient rsmarisa LOUDS trie.

use super::trie::TrieEntry;
use crate::error::SudachiResult;
use rsmarisa::grimoire::io::{Reader, Writer};
use rsmarisa::{Agent, Keyset};
use std::iter::FusedIterator;

/// A MARISA-backed trie for sudachi lexicon lookup.
///
/// Holds the rsmarisa trie and a parallel mapping from MARISA key IDs
/// to WordIdTable offsets. The MARISA trie assigns sequential IDs to keys
/// during build; `id_to_offset[key_id]` stores the corresponding offset
/// into the WordIdTable, matching the `value` semantics of the YADA trie.
pub struct MarisaTrie {
    trie: rsmarisa::Trie,
    /// Maps MARISA key_id → WordIdTable offset (same as TrieEntry::value).
    id_to_offset: Vec<u32>,
}

/// Iterator that yields [`TrieEntry`] items from a MARISA common-prefix search.
///
/// Wraps rsmarisa's stateful Agent-based iteration to provide the same
/// iterator interface as [`super::trie::TrieEntryIter`].
pub struct MarisaTrieEntryIter<'a> {
    trie: &'a rsmarisa::Trie,
    id_to_offset: &'a [u32],
    agent: Agent,
    /// Byte offset of the query start within the original input.
    base_offset: usize,
    done: bool,
}

impl Iterator for MarisaTrieEntryIter<'_> {
    type Item = TrieEntry;

    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        if self.trie.common_prefix_search(&mut self.agent) {
            let key_id = self.agent.key().id();
            let match_len = self.agent.key().length();
            let offset = self.id_to_offset[key_id];
            Some(TrieEntry::new(offset, self.base_offset + match_len))
        } else {
            self.done = true;
            None
        }
    }
}

impl FusedIterator for MarisaTrieEntryIter<'_> {}

impl MarisaTrie {
    /// Build a MARISA trie from (key, WordIdTable-offset) pairs.
    ///
    /// Keys are byte strings (UTF-8 surface forms). The offset values are
    /// stored in a side-array indexed by the MARISA-assigned key ID.
    pub fn build(entries: &[(&str, u32)]) -> SudachiResult<MarisaTrie> {
        let mut keyset = Keyset::new();
        for (key, _offset) in entries {
            keyset
                .push_back_str(key)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        }

        let mut trie = rsmarisa::Trie::new();
        trie.build(&mut keyset, 0);

        // Build the id_to_offset mapping.
        // After build, keyset[i].id() gives the MARISA-assigned key ID for entry i.
        let num_keys = trie.num_keys();
        let mut id_to_offset = vec![0u32; num_keys];
        for (i, (_key, offset)) in entries.iter().enumerate() {
            let key_id = keyset.get(i).id();
            id_to_offset[key_id] = *offset;
        }

        Ok(MarisaTrie { trie, id_to_offset })
    }

    /// Returns the total serialized size in bytes (trie IO size + mapping array).
    pub fn total_size(&self) -> usize {
        self.trie.io_size() + 4 + self.id_to_offset.len() * 4
    }

    /// Serialize the MARISA trie and offset mapping to bytes.
    pub fn to_bytes(&self) -> SudachiResult<Vec<u8>> {
        // Serialize the rsmarisa trie
        let mut writer = Writer::from_vec(Vec::new());
        self.trie
            .write(&mut writer)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let trie_bytes = writer
            .into_inner()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;

        let mut result = Vec::with_capacity(4 + trie_bytes.len() + 4 + self.id_to_offset.len() * 4);

        // [trie_bytes_len: u32]
        result.extend_from_slice(&(trie_bytes.len() as u32).to_le_bytes());
        // [trie_bytes: ...]
        result.extend_from_slice(&trie_bytes);
        // [id_map_len: u32]
        result.extend_from_slice(&(self.id_to_offset.len() as u32).to_le_bytes());
        // [id_map: u32 × len]
        for &offset in &self.id_to_offset {
            result.extend_from_slice(&offset.to_le_bytes());
        }

        Ok(result)
    }

    /// Deserialize from a byte buffer at the given offset.
    ///
    /// Returns the MarisaTrie and the number of bytes consumed.
    pub fn from_bytes(buf: &[u8], offset: usize) -> SudachiResult<(MarisaTrie, usize)> {
        let mut pos = offset;

        // Read trie_bytes_len
        if pos + 4 > buf.len() {
            return Err(crate::error::SudachiError::InvalidRange(pos, pos + 4));
        }
        let trie_bytes_len =
            u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        // Read trie bytes and deserialize
        if pos + trie_bytes_len > buf.len() {
            return Err(crate::error::SudachiError::InvalidRange(
                pos,
                pos + trie_bytes_len,
            ));
        }
        let trie_data = &buf[pos..pos + trie_bytes_len];
        let mut reader = Reader::from_bytes(trie_data);
        let mut trie = rsmarisa::Trie::new();
        trie.read(&mut reader)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        pos += trie_bytes_len;

        // Read id_map_len
        if pos + 4 > buf.len() {
            return Err(crate::error::SudachiError::InvalidRange(pos, pos + 4));
        }
        let id_map_len =
            u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;

        // Read id_map
        let id_map_bytes = id_map_len * 4;
        if pos + id_map_bytes > buf.len() {
            return Err(crate::error::SudachiError::InvalidRange(
                pos,
                pos + id_map_bytes,
            ));
        }
        let mut id_to_offset = Vec::with_capacity(id_map_len);
        for i in 0..id_map_len {
            let start = pos + i * 4;
            let val = u32::from_le_bytes(buf[start..start + 4].try_into().unwrap());
            id_to_offset.push(val);
        }
        pos += id_map_bytes;

        let consumed = pos - offset;
        Ok((MarisaTrie { trie, id_to_offset }, consumed))
    }

    /// Returns an iterator of matching entries for all prefixes of `input[offset..]`.
    #[inline]
    pub fn common_prefix_iterator<'a>(
        &'a self,
        input: &'a [u8],
        offset: usize,
    ) -> MarisaTrieEntryIter<'a> {
        let mut agent = Agent::new();
        agent.set_query_bytes(&input[offset..]);

        MarisaTrieEntryIter {
            trie: &self.trie,
            id_to_offset: &self.id_to_offset,
            agent,
            base_offset: offset,
            done: false,
        }
    }

    /// Access the id_to_offset mapping value for a given key ID.
    #[inline]
    pub fn id_to_offset_val(&self, key_id: usize) -> u32 {
        self.id_to_offset[key_id]
    }

    /// Predictive search: find all keys starting with `prefix`, returning
    /// (reading_string, word_ids) pairs using the provided word_id_table.
    pub fn predictive_search_entries(
        &self,
        prefix: &str,
        word_id_table: &super::word_id_table::WordIdTable,
    ) -> Vec<(String, Vec<u32>)> {
        let mut agent = Agent::new();
        agent.set_query_str(prefix);

        let mut results = Vec::new();
        while self.trie.predictive_search(&mut agent) {
            let key_str = std::str::from_utf8(agent.key().as_bytes())
                .unwrap_or_default()
                .to_string();
            let key_id = agent.key().id();
            let offset = self.id_to_offset[key_id];
            let word_ids: Vec<u32> = word_id_table.entries(offset as usize).collect();
            results.push((key_str, word_ids));
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_marisa_trie_build_and_lookup() {
        let entries = vec![("abc", 100u32), ("ab", 200), ("a", 300)];
        let trie = MarisaTrie::build(&entries).unwrap();

        let input = b"abcdef";
        let results: Vec<TrieEntry> = trie.common_prefix_iterator(input, 0).collect();
        assert_eq!(results.len(), 3);

        // Check that all three prefixes are found
        let mut ends: Vec<usize> = results.iter().map(|e| e.end).collect();
        ends.sort();
        assert_eq!(ends, vec![1, 2, 3]); // "a"=1, "ab"=2, "abc"=3

        // Check that values map correctly
        for entry in &results {
            match entry.end {
                1 => assert_eq!(entry.value, 300), // "a" → 300
                2 => assert_eq!(entry.value, 200), // "ab" → 200
                3 => assert_eq!(entry.value, 100), // "abc" → 100
                _ => panic!("unexpected end {}", entry.end),
            }
        }
    }

    #[test]
    fn test_marisa_trie_serialize_roundtrip() {
        let entries = vec![("hello", 10u32), ("help", 20), ("he", 30)];
        let trie = MarisaTrie::build(&entries).unwrap();

        let bytes = trie.to_bytes().unwrap();
        let (trie2, consumed) = MarisaTrie::from_bytes(&bytes, 0).unwrap();
        assert_eq!(consumed, bytes.len());

        let input = b"hello world";
        let r1: Vec<TrieEntry> = trie.common_prefix_iterator(input, 0).collect();
        let r2: Vec<TrieEntry> = trie2.common_prefix_iterator(input, 0).collect();
        assert_eq!(r1, r2);
    }

    #[test]
    fn test_marisa_trie_with_offset() {
        let entries = vec![("abc", 100u32), ("ab", 200)];
        let trie = MarisaTrie::build(&entries).unwrap();

        // Search starting at offset 3 in "xxxabc"
        let input = b"xxxabc";
        let results: Vec<TrieEntry> = trie.common_prefix_iterator(input, 3).collect();
        assert_eq!(results.len(), 2);

        let mut ends: Vec<usize> = results.iter().map(|e| e.end).collect();
        ends.sort();
        assert_eq!(ends, vec![5, 6]); // "ab" ends at 5, "abc" ends at 6
    }
}
