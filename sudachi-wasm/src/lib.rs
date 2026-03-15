/*
 * Copyright (c) 2024 Works Applications Co., Ltd.
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

use std::sync::Arc;

use serde::Serialize;
use wasm_bindgen::prelude::*;

use sudachi::analysis::stateless_tokenizer::StatelessTokenizer;
use sudachi::analysis::Tokenize;
use sudachi::dic::dictionary::JapaneseDictionary;
use sudachi::dic::grammar::Grammar;
use sudachi::dic::lexicon_set::LexiconSet;
use sudachi::dic::subset::InfoSubset;
use sudachi::kana::hiragana_to_katakana;
use sudachi::prelude::*;

/// Install a panic hook that forwards Rust panics to `console.error` in JS.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

/// Split granularity for tokenization.
///
/// - `"A"` – shortest units
/// - `"B"` – middle units
/// - `"C"` – named-entity / longest units (default)
fn parse_mode(s: &str) -> Mode {
    match s.to_uppercase().as_str() {
        "A" => Mode::A,
        "B" => Mode::B,
        _ => Mode::C,
    }
}

/// A single KKC candidate returned from [`Tokenizer::lookup_by_reading`].
///
/// Candidates are sorted by `cost` (ascending — lower cost = higher priority).
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct KkcCandidate {
    /// Surface form (the kanji/kana spelling to display as conversion result).
    surface: String,
    /// Dictionary (base) form — the lemma this surface inflects from.
    dictionary_form: String,
    /// Reading in katakana.
    reading_form: String,
    /// Part-of-speech components.
    part_of_speech: Vec<String>,
    /// Word cost (lower = preferred by the language model).
    cost: i16,
}

/// A single morpheme in a KKC conversion result from [`Tokenizer::kkc_convert`].
///
/// Contains both the original hiragana surface and the POS-aware KKC output,
/// plus the full candidate list for interactive candidate selection.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct KkcMorpheme {
    /// The KKC-converted surface (kanji/kana output).
    kkc_surface: String,
    /// The original surface from tokenization (hiragana input).
    original_surface: String,
    /// Reading in katakana.
    reading_form: String,
    /// Dictionary (base) form from the tokenizer.
    dictionary_form: String,
    /// Normalized form from the tokenizer (often the standard kanji form).
    normalized_form: String,
    /// Part-of-speech components from the tokenizer.
    part_of_speech: Vec<String>,
    /// `true` when the word was not found in any dictionary (out-of-vocabulary).
    is_oov: bool,
    /// Ranked KKC candidates (empty for functional POS that are kept as-is).
    candidates: Vec<KkcCandidate>,
    /// Start character offset in the original string.
    begin: usize,
    /// End character offset in the original string (exclusive).
    end: usize,
}

/// A single morpheme returned from [`Tokenizer::tokenize`].
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MorphemeData {
    /// Surface form as it appears in the input text.
    surface: String,
    /// Dictionary (base) form.
    dictionary_form: String,
    /// Reading in katakana.
    reading_form: String,
    /// Normalized form.
    normalized_form: String,
    /// Part-of-speech components (up to 6 fields, e.g. `["名詞","普通名詞","一般","*","*","*"]`).
    part_of_speech: Vec<String>,
    /// `true` when the word was not found in any dictionary (out-of-vocabulary).
    is_oov: bool,
    /// Start character offset in the original string (Unicode code-point index).
    begin: usize,
    /// End character offset in the original string (Unicode code-point index, exclusive).
    end: usize,
}

/// POS major categories for functional/grammatical words that should never be
/// converted to kanji in KKC — these are conventionally written in hiragana.
fn is_functional_pos(major_pos: &str) -> bool {
    matches!(
        major_pos,
        "助詞" | "助動詞" | "補助記号" | "記号" | "空白" | "接続詞" | "感動詞"
    )
}

/// Look up KKC candidates for a given katakana reading, keeping only the
/// lowest-cost entry per surface and returning candidates sorted by cost.
fn collect_kkc_candidates(
    lexicon: &LexiconSet<'_>,
    grammar: &Grammar<'_>,
    katakana: &str,
) -> Vec<KkcCandidate> {
    let mut best_by_surface: std::collections::HashMap<String, KkcCandidate> =
        std::collections::HashMap::new();
    for word_id in lexicon.lookup_by_reading(katakana) {
        let info = match lexicon.get_word_info_subset(word_id, InfoSubset::all()) {
            Ok(info) => info,
            Err(_) => continue,
        };
        let (_, _, cost) = lexicon.get_word_param(word_id);
        let surface = info.surface().to_string();
        let dominated = best_by_surface
            .get(&surface)
            .map_or(false, |prev| cost >= prev.cost);
        if dominated {
            continue;
        }
        best_by_surface.insert(
            surface.clone(),
            KkcCandidate {
                surface,
                dictionary_form: info.dictionary_form().to_string(),
                reading_form: info.reading_form().to_string(),
                part_of_speech: grammar.pos_components(info.pos_id()).to_vec(),
                cost,
            },
        );
    }
    let mut candidates: Vec<KkcCandidate> = best_by_surface.into_values().collect();
    candidates.sort_by_key(|c| c.cost);
    candidates
}

/// A Sudachi tokenizer that holds a loaded dictionary.
///
/// Construct with `new Tokenizer(dictBytes)` where `dictBytes` is a `Uint8Array`
/// containing the raw bytes of a Sudachi system dictionary (`.dic` file).
#[wasm_bindgen]
pub struct Tokenizer {
    inner: StatelessTokenizer<Arc<JapaneseDictionary>>,
}

#[wasm_bindgen]
impl Tokenizer {
    /// Create a tokenizer from the raw bytes of a Sudachi system dictionary.
    ///
    /// ```js
    /// const response = await fetch("system_core.dic");
    /// const dictBytes = new Uint8Array(await response.arrayBuffer());
    /// const tokenizer = new Tokenizer(dictBytes);
    /// ```
    #[wasm_bindgen(constructor)]
    pub fn new(dict_bytes: &[u8]) -> Result<Tokenizer, JsError> {
        let dict = Arc::new(
            JapaneseDictionary::from_system_bytes(dict_bytes.to_vec())
                .map_err(|e| JsError::new(&e.to_string()))?,
        );
        Ok(Tokenizer {
            inner: StatelessTokenizer::new(dict),
        })
    }

    /// Tokenize Japanese text and return an array of morpheme objects.
    ///
    /// @param text  - Input Japanese text.
    /// @param mode  - Split mode: `"A"` (short), `"B"` (middle), or `"C"` (default, named-entity).
    /// @returns     Array of `{ surface, dictionaryForm, readingForm, normalizedForm,
    ///              partOfSpeech, isOov, begin, end }` objects.
    ///
    /// ```js
    /// const morphemes = tokenizer.tokenize("今日はいい天気ですね。", "C");
    /// for (const m of morphemes) {
    ///   console.log(m.surface, m.readingForm, m.partOfSpeech.join("-"));
    /// }
    /// ```
    pub fn tokenize(&self, text: &str, mode: &str) -> Result<JsValue, JsError> {
        let result = self
            .inner
            .tokenize(text, parse_mode(mode), false)
            .map_err(|e| JsError::new(&e.to_string()))?;

        let morphemes: Vec<MorphemeData> = result
            .iter()
            .map(|m| MorphemeData {
                surface: m.surface().to_string(),
                dictionary_form: m.dictionary_form().to_string(),
                reading_form: m.reading_form().to_string(),
                normalized_form: m.normalized_form().to_string(),
                part_of_speech: m.part_of_speech().to_vec(),
                is_oov: m.is_oov(),
                begin: m.begin_c(),
                end: m.end_c(),
            })
            .collect();

        serde_wasm_bindgen::to_value(&morphemes).map_err(|e| JsError::new(&e.to_string()))
    }

    /// Look up all dictionary entries whose reading matches `hiragana` and return
    /// them as a ranked list of KKC candidates.
    ///
    /// This is the core of KKC (Kana-Kanji Conversion): given a hiragana reading,
    /// enumerate every word in the dictionary that has that reading, then sort by
    /// word cost so the most likely kanji surface appears first.
    ///
    /// @param hiragana - Hiragana reading to look up (e.g. `"あめ"`).
    /// @returns Array of `{ surface, dictionaryForm, readingForm, partOfSpeech, cost }`
    ///          sorted by cost ascending (lower = higher priority).
    ///
    /// ```js
    /// const candidates = tokenizer.lookup_by_reading("あめ");
    /// // → [{ surface: "雨", dictionaryForm: "雨", cost: 5432, … },
    /// //    { surface: "飴", dictionaryForm: "飴", cost: 6789, … }, …]
    /// ```
    pub fn lookup_by_reading(&self, hiragana: &str) -> Result<JsValue, JsError> {
        let dict = self.inner.as_dict();
        let katakana = hiragana_to_katakana(hiragana);
        let lexicon = dict.lexicon();
        let grammar = dict.grammar();

        let candidates = collect_kkc_candidates(lexicon, grammar, &katakana);

        serde_wasm_bindgen::to_value(&candidates).map_err(|e| JsError::new(&e.to_string()))
    }

    /// Perform POS-aware Kana-Kanji Conversion on hiragana text.
    ///
    /// The dictionary's main trie is reading-keyed (hiragana), so the Viterbi
    /// lattice search directly considers kanji candidates during segmentation
    /// and path selection. Each morpheme's word_info surface is the kanji form
    /// chosen by the language model (connection costs + word costs).
    ///
    /// Functional POS (助詞, 助動詞, etc.) naturally remain in hiragana because
    /// their word_info surfaces are hiragana.
    ///
    /// @param text  - Hiragana input text to convert.
    /// @param mode  - Split mode: `"A"`, `"B"`, or `"C"` (default).
    /// @returns     Array of `{ kkcSurface, originalSurface, readingForm,
    ///              dictionaryForm, normalizedForm, partOfSpeech, isOov,
    ///              candidates, begin, end }` objects.
    ///
    /// ```js
    /// const result = tokenizer.kkc_convert("きょうはいいてんきですね。", "C");
    /// const converted = result.map(m => m.kkcSurface).join("");
    /// // → "今日はいい天気ですね。"
    /// ```
    pub fn kkc_convert(&self, text: &str, mode: &str) -> Result<JsValue, JsError> {
        let result = self
            .inner
            .tokenize(text, parse_mode(mode), false)
            .map_err(|e| JsError::new(&e.to_string()))?;

        let dict = self.inner.as_dict();
        let lexicon = dict.lexicon();
        let grammar = dict.grammar();

        let morphemes: Vec<KkcMorpheme> = result
            .iter()
            .map(|m| {
                let pos = m.part_of_speech();
                let major_pos = pos.first().map(|s| s.as_str()).unwrap_or("");
                let surface = m.surface().to_string();
                let reading = m.reading_form().to_string();

                // Viterbi-selected kanji surface from word_info
                let kkc_surface = if m.is_oov() {
                    surface.clone()
                } else {
                    m.get_word_info().surface().to_string()
                };

                // Candidates for interactive selection (via RTRI)
                let candidates = if is_functional_pos(major_pos) {
                    vec![]
                } else {
                    collect_kkc_candidates(lexicon, grammar, &reading)
                };

                KkcMorpheme {
                    kkc_surface,
                    original_surface: surface,
                    reading_form: reading,
                    dictionary_form: m.dictionary_form().to_string(),
                    normalized_form: m.normalized_form().to_string(),
                    part_of_speech: pos.to_vec(),
                    is_oov: m.is_oov(),
                    candidates,
                    begin: m.begin_c(),
                    end: m.end_c(),
                }
            })
            .collect();

        serde_wasm_bindgen::to_value(&morphemes).map_err(|e| JsError::new(&e.to_string()))
    }
}
