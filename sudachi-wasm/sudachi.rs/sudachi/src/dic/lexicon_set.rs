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

use std::collections::BTreeMap;
use std::sync::OnceLock;

use thiserror::Error;

use crate::dic::lexicon::word_infos::{WordInfo, WordInfoData};
use crate::dic::lexicon::{Lexicon, LexiconEntry, MAX_DICTIONARIES};
use crate::dic::subset::InfoSubset;
use crate::dic::word_id::WordId;
use crate::kana::hiragana_to_katakana;
use crate::prelude::*;



/// Sudachi error
#[derive(Error, Debug, Eq, PartialEq)]
pub enum LexiconSetError {
    #[error("too large word_id {0} in dict {1}")]
    TooLargeWordId(u32, usize),

    #[error("too large dictionary_id {0}")]
    TooLargeDictionaryId(usize),

    #[error("too many user dictionaries")]
    TooManyDictionaries,
}

/// Set of Lexicons
///
/// Handles multiple lexicons as one lexicon
/// The first lexicon in the list must be from system dictionary
pub struct LexiconSet<'a> {
    lexicons: Vec<Lexicon<'a>>,
    pos_offsets: Vec<usize>,
    num_system_pos: usize,
    /// Lazy-built reverse index: katakana reading → list of matching `WordId`s.
    /// Built on first call to [`lookup_by_reading`] or [`lookup_by_reading_prefix`].
    reading_index: OnceLock<BTreeMap<String, Vec<WordId>>>,
}

impl<'a> LexiconSet<'a> {
    /// Creates a LexiconSet given a lexicon
    ///
    /// It is assumed that the passed lexicon is the system dictionary
    pub fn new(mut system_lexicon: Lexicon, num_system_pos: usize) -> LexiconSet {
        system_lexicon.set_dic_id(0);
        LexiconSet {
            lexicons: vec![system_lexicon],
            pos_offsets: vec![0],
            num_system_pos,
            reading_index: OnceLock::new(),
        }
    }

    /// Add a lexicon to the lexicon list
    ///
    /// pos_offset: number of pos in the grammar
    pub fn append(
        &mut self,
        mut lexicon: Lexicon<'a>,
        pos_offset: usize,
    ) -> Result<(), LexiconSetError> {
        if self.is_full() {
            return Err(LexiconSetError::TooManyDictionaries);
        }
        lexicon.set_dic_id(self.lexicons.len() as u8);
        self.lexicons.push(lexicon);
        self.pos_offsets.push(pos_offset);
        Ok(())
    }

    /// Returns if dictionary capacity is full
    pub fn is_full(&self) -> bool {
        self.lexicons.len() >= MAX_DICTIONARIES
    }
}

impl LexiconSet<'_> {
    /// Returns iterator which yields all words in the dictionary, starting from the `offset` bytes
    ///
    /// Searches dictionaries in the reverse order: user dictionaries first and then system dictionary
    #[inline]
    pub fn lookup<'b>(
        &'b self,
        input: &'b [u8],
        offset: usize,
    ) -> impl Iterator<Item = LexiconEntry> + 'b {
        // word_id fixup was moved to lexicon itself
        self.lexicons
            .iter()
            .rev()
            .flat_map(move |l| l.lookup(input, offset))
    }

    /// Returns WordInfo for given WordId
    pub fn get_word_info(&self, id: WordId) -> SudachiResult<WordInfo> {
        self.get_word_info_subset(id, InfoSubset::all())
    }

    /// Returns WordInfo for given WordId.
    /// Only fills a requested subset of fields.
    /// Rest will be of default values (0 or empty).
    pub fn get_word_info_subset(&self, id: WordId, subset: InfoSubset) -> SudachiResult<WordInfo> {
        let dict_id = id.dic();
        let mut word_info: WordInfoData = self.lexicons[dict_id as usize]
            .get_word_info(id.word(), subset)?
            .into();

        if subset.contains(InfoSubset::POS_ID) {
            let pos_id = word_info.pos_id as usize;
            if dict_id > 0 && pos_id >= self.num_system_pos {
                // user defined part-of-speech
                word_info.pos_id =
                    (pos_id - self.num_system_pos + self.pos_offsets[dict_id as usize]) as u16;
            }
        }

        if subset.contains(InfoSubset::SPLIT_A) {
            Self::update_dict_id(&mut word_info.a_unit_split, dict_id)?;
        }

        if subset.contains(InfoSubset::SPLIT_B) {
            Self::update_dict_id(&mut word_info.b_unit_split, dict_id)?;
        }

        if subset.contains(InfoSubset::WORD_STRUCTURE) {
            Self::update_dict_id(&mut word_info.word_structure, dict_id)?;
        }

        Ok(word_info.into())
    }

    /// Returns word_param for given word_id
    pub fn get_word_param(&self, id: WordId) -> (i16, i16, i16) {
        let dic_id = id.dic() as usize;
        self.lexicons[dic_id].get_word_param(id.word())
    }

    fn update_dict_id(split: &mut Vec<WordId>, dict_id: u8) -> SudachiResult<()> {
        for id in split.iter_mut() {
            let cur_dict_id = id.dic();
            if cur_dict_id > 0 {
                // update if target word is not in system_dict
                *id = WordId::checked(dict_id, id.word())?;
            }
        }
        Ok(())
    }

    pub fn size(&self) -> u32 {
        self.lexicons.iter().fold(0, |acc, lex| acc + lex.size())
    }
}

impl LexiconSet<'_> {
    /// Return (or lazily build) the reverse reading index.
    ///
    /// The index maps katakana reading strings to the list of `WordId`s that
    /// carry that reading.  It is built once on first use and covers every
    /// lexicon present in the set at that time.
    ///
    /// When a pre-built reading trie is available, this is only used as a
    /// fallback for user dictionaries that were appended after the system dict.
    fn get_or_build_reading_index(&self) -> &BTreeMap<String, Vec<WordId>> {
        self.reading_index.get_or_init(|| {
            let mut index: BTreeMap<String, Vec<WordId>> = BTreeMap::new();
            for lexicon in &self.lexicons {
                // Skip system lexicon if it has a reading trie — those entries
                // are already covered by the pre-built trie.
                #[cfg(feature = "marisa-trie")]
                if lexicon.has_reading_trie() {
                    continue;
                }
                for (word_id, result) in lexicon.iter_reading_words() {
                    if let Ok(info) = result {
                        // Normalize to katakana: reading_form() falls back to
                        // surface when empty, which may be hiragana.
                        let reading = hiragana_to_katakana(info.reading_form());
                        index.entry(reading).or_default().push(word_id);
                    }
                }
            }
            index
        })
    }

    /// Returns true if the system lexicon has a pre-built reading trie.
    #[cfg(feature = "marisa-trie")]
    fn has_reading_trie(&self) -> bool {
        self.lexicons
            .first()
            .map(|l| l.has_reading_trie())
            .unwrap_or(false)
    }

    /// Look up all words whose reading exactly matches `katakana`.
    ///
    /// `katakana` should be a katakana string (see [`crate::kana::hiragana_to_katakana`]
    /// to convert hiragana input before calling this method).
    ///
    /// Returns an iterator of [`WordId`]s.  Call [`LexiconSet::get_word_info`] on each
    /// to retrieve the full [`WordInfo`].
    pub fn lookup_by_reading<'s>(&'s self, katakana: &str) -> impl Iterator<Item = WordId> + 's {
        #[cfg(feature = "marisa-trie")]
        {
            // Use reading trie for the system lexicon
            let trie_ids: Vec<WordId> = if let Some(rt) = self.lexicons.first().and_then(|l| l.reading_trie()) {
                rt.lookup_exact(katakana)
                    .into_iter()
                    .map(|raw| WordId::new(0, raw))
                    .collect()
            } else {
                Vec::new()
            };

            // BTreeMap fallback for lexicons without a reading trie (user dicts)
            let index = self.get_or_build_reading_index();
            let btree_ids = index
                .get(katakana)
                .map(|ids| ids.as_slice())
                .unwrap_or_default()
                .iter()
                .copied();

            return trie_ids.into_iter().chain(btree_ids);
        }

        #[cfg(not(feature = "marisa-trie"))]
        {
            let index = self.get_or_build_reading_index();
            let ids = index
                .get(katakana)
                .map(|ids| ids.as_slice())
                .unwrap_or_default()
                .iter()
                .copied();
            ids.chain(std::iter::empty())
        }
    }

    /// Look up all words whose reading starts with `katakana_prefix`.
    ///
    /// Useful for progressive IME candidate generation as the user types.
    /// `katakana_prefix` should be a katakana string (see
    /// [`crate::kana::hiragana_to_katakana`]).
    ///
    /// Returns an iterator of [`WordId`]s ordered by reading.
    pub fn lookup_by_reading_prefix<'s>(
        &'s self,
        katakana_prefix: &'s str,
    ) -> impl Iterator<Item = WordId> + 's {
        #[cfg(feature = "marisa-trie")]
        {
            let trie_ids: Vec<WordId> = if let Some(rt) = self.lexicons.first().and_then(|l| l.reading_trie()) {
                rt.lookup_prefix(katakana_prefix)
                    .into_iter()
                    .flat_map(|(_reading, wids)| wids.into_iter().map(|raw| WordId::new(0, raw)))
                    .collect()
            } else {
                Vec::new()
            };

            let index = self.get_or_build_reading_index();
            let btree_ids = index
                .range(katakana_prefix.to_owned()..)
                .take_while(move |(k, _)| k.starts_with(katakana_prefix))
                .flat_map(|(_, ids)| ids.iter().copied());

            return trie_ids.into_iter().chain(btree_ids);
        }

        #[cfg(not(feature = "marisa-trie"))]
        {
            let index = self.get_or_build_reading_index();
            index
                .range(katakana_prefix.to_owned()..)
                .take_while(move |(k, _)| k.starts_with(katakana_prefix))
                .flat_map(|(_, ids)| ids.iter().copied())
                .chain(std::iter::empty())
        }
    }

    /// Common-prefix search over katakana bytes for lattice construction.
    ///
    /// Given `katakana_bytes` starting at `offset`, yields all readings that
    /// are prefixes of the input. Each result contains the byte end position
    /// and the list of WordIds with that reading.
    ///
    /// This is the key optimization for KKC: instead of O(n²) BTreeMap lookups
    /// in `fill_system_entries`, a single common-prefix search per start
    /// position traverses the trie in O(L) time.
    ///
    /// Falls back to None if no reading trie is available (caller should use
    /// the O(n²) BTreeMap approach in that case).
    #[cfg(feature = "marisa-trie")]
    pub fn common_prefix_by_reading(
        &self,
        katakana_bytes: &[u8],
        offset: usize,
    ) -> Option<Vec<(usize, Vec<WordId>)>> {
        let rt = self.lexicons.first()?.reading_trie()?;
        let matches = rt.common_prefix_search(katakana_bytes, offset);
        Some(
            matches
                .into_iter()
                .map(|m| {
                    let word_ids: Vec<WordId> = m
                        .word_ids
                        .into_iter()
                        .map(|raw| WordId::new(0, raw))
                        .collect();
                    (m.end, word_ids)
                })
                .collect(),
        )
    }

    /// Eagerly build the reverse reading index.
    ///
    /// Call this during application initialization (on a background thread) to
    /// avoid a freeze on the first `lookup_by_reading` / `lookup_by_reading_prefix`
    /// call.
    ///
    /// When a reading trie is available, this is a no-op (no BTreeMap needed).
    pub fn warmup_reading_index(&self) {
        #[cfg(feature = "marisa-trie")]
        if self.has_reading_trie() {
            return;
        }
        let _ = self.get_or_build_reading_index();
    }
}
