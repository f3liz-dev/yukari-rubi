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

use std::cmp;
#[cfg(not(feature = "marisa-trie"))]
use std::mem::size_of;

use crate::analysis::stateful_tokenizer::StatefulTokenizer;
use crate::analysis::stateless_tokenizer::DictionaryAccess;
use crate::dic::subset::InfoSubset;
use crate::dic::word_id::WordId;
use nom::{bytes::complete::take, number::complete::le_u32};

use crate::error::SudachiNomResult;
use crate::prelude::*;

#[cfg(not(feature = "marisa-trie"))]
use self::trie::Trie;
use self::word_id_table::WordIdTable;
use self::word_infos::{WordInfo, WordInfos};
use self::word_params::WordParams;

pub mod trie;
pub mod word_id_table;
pub mod word_infos;
pub mod word_params;

#[cfg(feature = "marisa-trie")]
pub mod marisa_trie;

#[cfg(feature = "marisa-trie")]
pub mod reading_trie;

/// The first 4 bits of word_id are used to indicate that from which lexicon
/// the word comes, thus we can only hold 15 lexicons in the same time.
/// 16th is reserved for marking OOVs.
pub const MAX_DICTIONARIES: usize = 15;

/// Dictionary lexicon
///
/// Contains trie, word_id, word_param, word_info, and optionally a reading trie
pub struct Lexicon<'a> {
    #[cfg(not(feature = "marisa-trie"))]
    trie: Trie<'a>,
    #[cfg(feature = "marisa-trie")]
    trie: marisa_trie::MarisaTrie,
    word_id_table: WordIdTable<'a>,
    word_params: WordParams<'a>,
    word_infos: WordInfos<'a>,
    lex_id: u8,
    #[cfg(feature = "marisa-trie")]
    reading_trie: Option<reading_trie::ReadingTrie<'a>>,
}

/// Result of the Lexicon lookup
#[derive(Eq, PartialEq, Debug)]
pub struct LexiconEntry {
    /// Id of the returned word
    pub word_id: WordId,
    /// Byte index of the word end
    pub end: usize,
}

impl LexiconEntry {
    pub fn new(word_id: WordId, end: usize) -> LexiconEntry {
        LexiconEntry { word_id, end }
    }
}

impl<'a> Lexicon<'a> {
    const USER_DICT_COST_PER_MORPH: i32 = -20;

    /// Parse a lexicon from the YADA-based dictionary binary format.
    #[cfg(not(feature = "marisa-trie"))]
    pub fn parse(
        buf: &[u8],
        original_offset: usize,
        has_synonym_group_ids: bool,
    ) -> SudachiResult<Lexicon> {
        let mut offset = original_offset;

        let (_rest, trie_size) = u32_parser_offset(buf, offset)?;
        offset += 4;
        let trie_array = trie_array_parser(buf, offset, trie_size)?;
        let trie = Trie::new(trie_array, trie_size as usize);
        offset += trie.total_size();

        let (_rest, word_id_table_size) = u32_parser_offset(buf, offset)?;
        let word_id_table = WordIdTable::new(buf, word_id_table_size, offset + 4);
        offset += word_id_table.storage_size();

        let (_rest, word_params_size) = u32_parser_offset(buf, offset)?;
        let word_params = WordParams::new(buf, word_params_size, offset + 4);
        offset += word_params.storage_size();

        let word_infos = WordInfos::new(buf, offset, word_params.size(), has_synonym_group_ids)?;

        Ok(Lexicon {
            trie,
            word_id_table,
            word_params,
            word_infos,
            lex_id: u8::MAX,
        })
    }

    /// Parse a lexicon from the MARISA-based dictionary binary format.
    #[cfg(feature = "marisa-trie")]
    pub fn parse(
        buf: &[u8],
        original_offset: usize,
        has_synonym_group_ids: bool,
    ) -> SudachiResult<Lexicon> {
        let mut offset = original_offset;

        let (trie, consumed) = marisa_trie::MarisaTrie::from_bytes(buf, offset)?;
        offset += consumed;

        let (_rest, word_id_table_size) = u32_parser_offset(buf, offset)?;
        let word_id_table = WordIdTable::new(buf, word_id_table_size, offset + 4);
        offset += word_id_table.storage_size();

        let (_rest, word_params_size) = u32_parser_offset(buf, offset)?;
        let word_params = WordParams::new(buf, word_params_size, offset + 4);
        offset += word_params.storage_size();

        let word_infos_offset = offset;
        let word_infos = WordInfos::new(buf, offset, word_params.size(), has_synonym_group_ids)?;

        // Try to detect and load reading trie after word_infos section.
        // Compute word_infos section size, then check for RTRI magic.
        let reading_trie = Self::try_load_reading_trie(buf, word_infos_offset, word_params.size());

        Ok(Lexicon {
            trie,
            word_id_table,
            word_params,
            word_infos,
            lex_id: u8::MAX,
            reading_trie,
        })
    }

    /// Assign lexicon id to the current Lexicon
    pub fn set_dic_id(&mut self, id: u8) {
        assert!(id < MAX_DICTIONARIES as u8);
        self.lex_id = id
    }

    #[inline]
    fn word_id(&self, raw_id: u32) -> WordId {
        WordId::new(self.lex_id, raw_id)
    }

    /// Returns an iterator of word_id and end of words that matches given input
    #[inline]
    pub fn lookup(
        &'a self,
        input: &'a [u8],
        offset: usize,
    ) -> impl Iterator<Item = LexiconEntry> + 'a {
        debug_assert!(self.lex_id < MAX_DICTIONARIES as u8);
        self.trie
            .common_prefix_iterator(input, offset)
            .flat_map(move |e| {
                self.word_id_table
                    .entries(e.value as usize)
                    .map(move |wid| LexiconEntry::new(self.word_id(wid), e.end))
            })
    }

    /// Returns WordInfo for given word_id
    ///
    /// WordInfo will contain only fields included in InfoSubset
    pub fn get_word_info(&self, word_id: u32, subset: InfoSubset) -> SudachiResult<WordInfo> {
        self.word_infos.get_word_info(word_id, subset)
    }

    /// Returns word_param for given word_id.
    /// Params are (left_id, right_id, cost).
    #[inline]
    pub fn get_word_param(&self, word_id: u32) -> (i16, i16, i16) {
        self.word_params.get_params(word_id)
    }

    /// update word_param cost based on current tokenizer
    pub fn update_cost<D: DictionaryAccess>(&mut self, dict: &D) -> SudachiResult<()> {
        let mut tok = StatefulTokenizer::create(dict, false, Mode::C);
        let mut ms = MorphemeList::empty(dict);
        for wid in 0..self.word_params.size() {
            if self.word_params.get_cost(wid) != i16::MIN {
                continue;
            }
            let wi = self.get_word_info(wid, InfoSubset::SURFACE)?;
            tok.reset().push_str(wi.surface());
            tok.do_tokenize()?;
            ms.collect_results(&mut tok)?;
            let internal_cost = ms.get_internal_cost();
            let cost = internal_cost + Lexicon::USER_DICT_COST_PER_MORPH * ms.len() as i32;
            let cost = cmp::min(cost, i16::MAX as i32);
            let cost = cmp::max(cost, i16::MIN as i32);
            self.word_params.set_cost(wid, cost as i16);
        }

        Ok(())
    }

    pub fn size(&self) -> u32 {
        self.word_params.size()
    }

    /// Returns the reading trie, if one was loaded from the dictionary.
    #[cfg(feature = "marisa-trie")]
    pub fn reading_trie(&self) -> Option<&reading_trie::ReadingTrie<'a>> {
        self.reading_trie.as_ref()
    }

    /// Returns true if this lexicon has a pre-built reading trie.
    #[cfg(feature = "marisa-trie")]
    pub fn has_reading_trie(&self) -> bool {
        self.reading_trie.is_some()
    }

    /// Try to load a reading trie from the dictionary bytes after the word_infos
    /// section. Returns None if no reading trie is found (backward compatible).
    #[cfg(feature = "marisa-trie")]
    fn try_load_reading_trie(
        buf: &[u8],
        word_infos_offset: usize,
        num_words: u32,
    ) -> Option<reading_trie::ReadingTrie> {
        // Compute word_infos section size by parsing its header.
        let wi_size = Self::compute_word_infos_size(buf, word_infos_offset, num_words)?;
        let reading_trie_offset = word_infos_offset + wi_size;

        // Check if there's enough room for the RTRI magic
        if reading_trie_offset + 4 > buf.len() {
            return None;
        }

        let magic = u32::from_le_bytes(
            buf[reading_trie_offset..reading_trie_offset + 4]
                .try_into()
                .ok()?,
        );
        if magic != reading_trie::READING_TRIE_MAGIC {
            return None;
        }

        match reading_trie::ReadingTrie::from_bytes(buf, reading_trie_offset) {
            Ok((rt, _consumed)) => Some(rt),
            Err(_) => None,
        }
    }

    /// Compute the total byte size of a block-compressed word_infos section.
    #[cfg(feature = "marisa-trie")]
    fn compute_word_infos_size(
        buf: &[u8],
        offset: usize,
        num_words: u32,
    ) -> Option<usize> {
        const BLOCK_WI_MAGIC: u32 = 0x4D57_4942; // "MWIB"

        if offset + 4 > buf.len() {
            return None;
        }
        let magic = u32::from_le_bytes(buf[offset..offset + 4].try_into().ok()?);
        if magic != BLOCK_WI_MAGIC {
            // Uncompressed word_infos — extends to end of buffer (no reading trie)
            return None;
        }

        let mut pos = offset + 4;
        let _nw = u32::from_le_bytes(buf[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4;
        let _records_per_block = u16::from_le_bytes(buf[pos..pos + 2].try_into().ok()?) as usize;
        pos += 2;
        let num_blocks = u32::from_le_bytes(buf[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4;
        let dict_size = u32::from_le_bytes(buf[pos..pos + 4].try_into().ok()?) as usize;
        pos += 4 + dict_size;
        pos += num_words as usize * 4;
        let block_index_start = pos;
        pos += num_blocks * 8;
        let total_compressed = if num_blocks > 0 {
            let last_bi = block_index_start + (num_blocks - 1) * 8;
            let last_offset =
                u32::from_le_bytes(buf[last_bi..last_bi + 4].try_into().ok()?) as usize;
            let last_size =
                u32::from_le_bytes(buf[last_bi + 4..last_bi + 8].try_into().ok()?) as usize;
            last_offset + last_size
        } else {
            0
        };
        pos += total_compressed;

        Some(pos - offset)
    }

    /// Iterate over all words in this lexicon, yielding `(WordId, reading_form)`.
    ///
    /// Used to build the reverse reading index for IME support.
    /// Only the `surface` and `reading_form` fields are fetched from each entry.
    pub fn iter_reading_words(
        &self,
    ) -> impl Iterator<Item = (WordId, SudachiResult<WordInfo>)> + '_ {
        let count = self.size();
        (0..count).map(move |wid| {
            let word_id = self.word_id(wid);
            let info = self
                .get_word_info(wid, InfoSubset::SURFACE | InfoSubset::READING_FORM | InfoSubset::DIC_FORM_WORD_ID);
            (word_id, info)
        })
    }
}

fn u32_parser_offset(input: &[u8], offset: usize) -> SudachiNomResult<&[u8], u32> {
    nom::sequence::preceded(take(offset), le_u32)(input)
}

#[cfg(not(feature = "marisa-trie"))]
fn trie_array_parser(input: &[u8], offset: usize, trie_size: u32) -> SudachiResult<&[u8]> {
    let trie_start = offset;
    let trie_end = offset + (trie_size as usize) * size_of::<u32>();
    if input.len() < trie_start {
        return Err(SudachiError::InvalidRange(trie_start, trie_end));
    }
    if input.len() < trie_end {
        return Err(SudachiError::InvalidRange(trie_start, trie_end));
    }
    let trie_data = &input[trie_start..trie_end];
    Ok(trie_data)
}
