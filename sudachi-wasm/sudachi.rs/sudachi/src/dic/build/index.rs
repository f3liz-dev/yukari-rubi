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

use crate::dic::build::error::{BuildFailure, DicBuildError};
use crate::dic::build::primitives::write_u32_array;
use crate::dic::word_id::WordId;
use crate::error::{SudachiError, SudachiResult};
use crate::util::fxhash::FxBuildHasher;
use indexmap::map::IndexMap;

pub struct IndexEntry {
    ids: Vec<WordId>,
    offset: usize,
}

impl Default for IndexEntry {
    fn default() -> Self {
        Self {
            ids: Vec::new(),
            offset: usize::MAX,
        }
    }
}

pub struct IndexBuilder<'a> {
    // Insertion order matters for the built dictionary,
    // so using IndexMap here instead of a simple HashMap
    data: IndexMap<&'a str, IndexEntry, FxBuildHasher>,
}

impl<'a> IndexBuilder<'a> {
    pub fn new() -> Self {
        Self {
            data: IndexMap::default(),
        }
    }

    pub fn add(&mut self, key: &'a str, id: WordId) {
        self.data.entry(key).or_default().ids.push(id)
    }

    pub fn build_word_id_table(&mut self) -> SudachiResult<Vec<u8>> {
        // by default assume that there will be 3 entries on average
        let mut result = Vec::with_capacity(self.data.len() * 13);
        for (k, entry) in self.data.iter_mut() {
            entry.offset = result.len();
            // clear stored ids memory after use
            let ids = std::mem::take(&mut entry.ids);
            write_u32_array(&mut result, &ids).map_err(|e| {
                SudachiError::DictionaryCompilationError(DicBuildError {
                    cause: e,
                    line: 0,
                    file: format!("<word id table for `{}` has too much entries>", k),
                })
            })?;
        }
        Ok(result)
    }

    /// Build a YADA double-array trie from the index entries.
    #[cfg(not(feature = "marisa-trie"))]
    pub fn build_trie(&mut self) -> SudachiResult<Vec<u8>> {
        let mut trie_entries: Vec<(&str, u32)> = Vec::new();
        for (k, v) in self.data.drain(..) {
            if v.offset > u32::MAX as _ {
                return Err(DicBuildError {
                    file: format!("entry {}", k),
                    line: 0,
                    cause: BuildFailure::WordIdTableNotBuilt,
                }
                .into());
            }
            trie_entries.push((k, v.offset as u32));
        }
        self.data.shrink_to_fit();
        trie_entries.sort_by(|(a, _), (b, _)| a.cmp(b));

        let trie = yada::builder::DoubleArrayBuilder::build(&trie_entries);
        match trie {
            Some(t) => Ok(t),
            None => Err(DicBuildError {
                file: "<trie>".to_owned(),
                line: 0,
                cause: BuildFailure::TrieBuildFailure,
            }
            .into()),
        }
    }

    /// Build a MARISA trie and serialize it (trie bytes + ID-to-offset mapping).
    #[cfg(feature = "marisa-trie")]
    pub fn build_trie(&mut self) -> SudachiResult<Vec<u8>> {
        use crate::dic::lexicon::marisa_trie::MarisaTrie;

        let mut trie_entries: Vec<(&str, u32)> = Vec::new();
        for (k, v) in self.data.drain(..) {
            if v.offset > u32::MAX as _ {
                return Err(DicBuildError {
                    file: format!("entry {}", k),
                    line: 0,
                    cause: BuildFailure::WordIdTableNotBuilt,
                }
                .into());
            }
            trie_entries.push((k, v.offset as u32));
        }
        self.data.shrink_to_fit();

        let marisa = MarisaTrie::build(&trie_entries).map_err(|_| DicBuildError {
            file: "<marisa-trie>".to_owned(),
            line: 0,
            cause: BuildFailure::TrieBuildFailure,
        })?;

        marisa.to_bytes().map_err(|_| {
            DicBuildError {
                file: "<marisa-trie-serialize>".to_owned(),
                line: 0,
                cause: BuildFailure::TrieBuildFailure,
            }
            .into()
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::dic::lexicon::trie::TrieEntry;

    #[cfg(not(feature = "marisa-trie"))]
    mod yada_tests {
        use super::*;
        use crate::dic::lexicon::trie::Trie;
        use std::convert::TryInto;

        fn make_trie(data: Vec<u8>) -> Trie<'static> {
            let mut elems: Vec<u32> = Vec::with_capacity(data.len() / 4);
            for i in (0..data.len()).step_by(4) {
                let arr: [u8; 4] = data[i..i + 4].try_into().unwrap();
                elems.push(u32::from_le_bytes(arr))
            }
            Trie::new_owned(elems)
        }

        #[test]
        fn build_index_1() {
            let mut bldr = IndexBuilder::new();
            bldr.add("test", WordId::new(0, 0));
            let _ = bldr.build_word_id_table().unwrap();

            let trie = make_trie(bldr.build_trie().unwrap());
            let mut iter = trie.common_prefix_iterator(b"test", 0);
            assert_eq!(iter.next(), Some(TrieEntry { value: 0, end: 4 }));
            assert_eq!(iter.next(), None);
        }

        #[test]
        fn build_index_2() {
            let mut bldr = IndexBuilder::new();
            bldr.add("test", WordId::new(0, 0));
            bldr.add("tes", WordId::new(0, 1));
            let _ = bldr.build_word_id_table().unwrap();

            let trie = make_trie(bldr.build_trie().unwrap());
            let mut iter = trie.common_prefix_iterator(b"test", 0);
            assert_eq!(iter.next(), Some(TrieEntry { value: 5, end: 3 }));
            assert_eq!(iter.next(), Some(TrieEntry { value: 0, end: 4 }));
            assert_eq!(iter.next(), None);
        }
    }

    #[cfg(feature = "marisa-trie")]
    mod marisa_tests {
        use super::*;
        use crate::dic::lexicon::marisa_trie::MarisaTrie;

        #[test]
        fn build_index_marisa_1() {
            let mut bldr = IndexBuilder::new();
            bldr.add("test", WordId::new(0, 0));
            let _ = bldr.build_word_id_table().unwrap();

            let trie_bytes = bldr.build_trie().unwrap();
            let (trie, _) = MarisaTrie::from_bytes(&trie_bytes, 0).unwrap();

            let results: Vec<TrieEntry> = trie.common_prefix_iterator(b"test", 0).collect();
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].value, 0);
            assert_eq!(results[0].end, 4);
        }

        #[test]
        fn build_index_marisa_2() {
            let mut bldr = IndexBuilder::new();
            bldr.add("test", WordId::new(0, 0));
            bldr.add("tes", WordId::new(0, 1));
            let _ = bldr.build_word_id_table().unwrap();

            let trie_bytes = bldr.build_trie().unwrap();
            let (trie, _) = MarisaTrie::from_bytes(&trie_bytes, 0).unwrap();

            let results: Vec<TrieEntry> = trie.common_prefix_iterator(b"test", 0).collect();
            assert_eq!(results.len(), 2);

            // Both "tes" and "test" should be found, values mapping to their offsets
            let mut entries: Vec<(usize, u32)> =
                results.iter().map(|e| (e.end, e.value)).collect();
            entries.sort_by_key(|&(end, _)| end);
            assert_eq!(entries[0].0, 3); // "tes" ends at 3
            assert_eq!(entries[0].1, 5); // offset 5
            assert_eq!(entries[1].0, 4); // "test" ends at 4
            assert_eq!(entries[1].1, 0); // offset 0
        }
    }
}
