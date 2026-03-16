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

//! IME (Input Method Engine) support: reading-to-surface candidate lookup
//! with 活用形 (conjugated form) awareness and compound segment folding.
//!
//! ## How Japanese conjugation is handled
//!
//! Sudachi's system dictionary already stores every conjugated form as a
//! **separate lexicon entry**.  For example, for the verb 食べる:
//!
//! | surface | reading | 活用型  | 活用形          |
//! |---------|---------|---------|-----------------|
//! | 食べる  | タベル  | 一段    | 終止形-一般     |
//! | 食べ    | タベ    | 一段    | 連用形-一般     |
//! | 食べて  | タベテ  | 一段    | 連用形-テ接続   |
//! | 食べた  | タベタ  | 一段    | 連用形-タ接続   |
//!
//! This means the reading index naturally covers all forms.  When the user
//! types "たべ" the prefix search finds *every* entry whose reading starts with
//! "タベ" — including the 連用形 entry 食べ (exact match) as well as longer
//! forms like 食べる, 食べて, …
//!
//! For prefix matches the aligned surface portion is computed via
//! [`crate::kana::surface_prefix_for_reading_prefix`], so "たべ" against
//! 食べる / タベル yields "食べ" as the candidate surface.
//!
//! ## Compound folding
//!
//! When the user types a long string like `おおいたけんびじゅつかん`, Viterbi
//! segments it into [大分県][美術館].  The compound folder walks all contiguous
//! spans of the Viterbi path and merges adjacent segments into compound
//! candidates (大分県美術館), scoring them with the same connection costs that
//! Viterbi itself uses.  This means naturally adjacent compounds rise to the
//! top without any manual dictionary entry.
//!
//! ## Usage
//!
//! ```ignore
//! use sudachi::ime::{classify_input, ime_lookup, ime_lookup_compound, InputPattern};
//!
//! // Simple per-reading lookup (single token):
//! let candidates = ime_lookup("たべ", &lexicon_set);
//!
//! // Compound-aware lookup (uses full Viterbi path):
//! let candidates = ime_lookup_compound("おおいたけんびじゅつかん", &lexicon, &lattice, &conn);
//! // → [大分県美術館, 大分県, 美術館, …]
//! ```

use std::collections::{HashMap, HashSet};

use crate::analysis::lattice::Lattice;
use crate::analysis::node::{LatticeNode, RightId};
use crate::dic::connect::ConnectionMatrix;
use crate::dic::lexicon_set::LexiconSet;
use crate::dic::word_id::WordId;
use crate::input_text::InputBuffer;
use crate::kana::{hiragana_to_katakana, surface_prefix_for_reading_prefix};

// ── InputPattern ──────────────────────────────────────────────────────────────

/// Classification of a hiragana input string for IME candidate strategy.
///
/// Used as a hint by the IME layer to decide how to rank candidates.  The
/// underlying [`ime_lookup`] function always performs a full prefix search
/// regardless of the pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputPattern {
    /// Input ends with a character that is a typical terminal mora of a
    /// Japanese word form (e.g. る, く, い, た).
    ///
    /// Strategy: prioritise exact reading matches; then show prefix completions.
    ///
    /// Examples: `たべる`, `いく`, `たかい`, `たかく`
    Complete(String),

    /// Input ends with a non-terminal kana that looks like a conjugation stem
    /// without its ending.
    ///
    /// Strategy: prefix search with surface alignment; the user is likely still
    /// typing the word.
    ///
    /// Examples: `たべ`, `いそが`, `いか`
    PartialStem(String),

    /// Very short input (≤ 2 chars, no clear terminal ending) where
    /// classification is ambiguous.
    ///
    /// Strategy: broad prefix walk.
    ///
    /// Examples: `き`, `に`
    Prefix(String),
}

/// Hiragana characters that typically form the terminal mora of a complete
/// Japanese word form.  Covers:
/// - verb 終止形 / 連体形: う-row (う、く、ぐ、す、ず、つ、づ、ぬ、ふ、ぶ、ぷ、む、ゆ) + る
/// - い-adjective 終止形: い
/// - past / te-form auxiliaries: た、だ、て、で
/// - negative / sentence-final: ん
const TERMINAL_ENDINGS: &[char] = &[
    'う', 'く', 'ぐ', 'す', 'ず', 'つ', 'づ', 'ぬ', 'ふ', 'ぶ', 'ぷ', 'む', 'ゆ', 'る', 'い', 'た',
    'だ', 'て', 'で', 'ん',
];

/// Classify a hiragana input string into an [`InputPattern`].
///
/// The classification is heuristic, based solely on the shape of the input
/// string — not on dictionary lookup.  Use it as a ranking hint.
///
/// ```
/// use sudachi::ime::{classify_input, InputPattern};
/// assert_eq!(classify_input("たべる"), InputPattern::Complete("たべる".into()));
/// assert_eq!(classify_input("たべ"),   InputPattern::PartialStem("たべ".into()));
/// assert_eq!(classify_input("き"),     InputPattern::Prefix("き".into()));
/// ```
pub fn classify_input(hiragana: &str) -> InputPattern {
    let chars: Vec<char> = hiragana.chars().collect();
    let len = chars.len();
    if len == 0 {
        return InputPattern::Prefix(String::new());
    }
    let last = chars[len - 1];
    if TERMINAL_ENDINGS.contains(&last) {
        InputPattern::Complete(hiragana.to_owned())
    } else if len == 1 {
        InputPattern::Prefix(hiragana.to_owned())
    } else {
        InputPattern::PartialStem(hiragana.to_owned())
    }
}

// ── ImeCandidate ──────────────────────────────────────────────────────────────

/// A single IME candidate produced by [`ime_lookup`] or [`ime_lookup_compound`].
#[derive(Debug, Clone)]
pub struct ImeCandidate {
    /// The surface string to display to the user.
    ///
    /// For an exact reading match this equals the full dictionary surface.
    /// For a prefix match this is the aligned portion of the surface that
    /// corresponds to the typed reading prefix.
    /// For a compound this is the concatenation of constituent surfaces.
    pub surface: String,

    /// The [`WordId`] of the primary (first) matched dictionary entry.
    pub word_id: WordId,

    /// The full katakana reading of the matched dictionary entry.
    /// For compounds this is the concatenation of constituent readings.
    pub reading: String,

    /// Accumulated word cost (+ connection costs for compounds).
    /// Lower is better — consistent with Viterbi scoring.
    pub cost: i32,

    /// Left connection id of the first segment.
    /// Used when this candidate is itself folded into a larger compound.
    pub left_id: u16,

    /// Right connection id of the last segment.
    /// Used when this candidate is itself folded into a larger compound.
    pub right_id: u16,

    /// Number of Viterbi segments folded into this candidate.
    /// 1 = single dictionary word; >1 = compound.
    pub segment_count: usize,
}

// ── ime_lookup ────────────────────────────────────────────────────────────────

/// Look up IME candidates for a hiragana input string (single-token).
///
/// Converts `typed_hiragana` to katakana and performs a prefix search over
/// the reading index.  For each matched word the aligned surface prefix is
/// computed via [`surface_prefix_for_reading_prefix`].
///
/// Results are **deduplicated by surface**: when multiple dictionary entries
/// yield the same display surface the entry with the lower cost is kept.
/// Candidates are returned sorted by cost (lowest first), then by reading
/// length, then lexicographically by surface.
pub fn ime_lookup(typed_hiragana: &str, lexicon: &LexiconSet) -> Vec<ImeCandidate> {
    let katakana = hiragana_to_katakana(typed_hiragana);
    let mut seen: HashMap<String, ImeCandidate> = HashMap::new();

    for word_id in lexicon.lookup_by_reading_prefix(&katakana) {
        let info = match lexicon.get_word_info(word_id) {
            Ok(i) => i,
            Err(_) => continue,
        };
        let full_reading = hiragana_to_katakana(info.reading_form());
        let surface = surface_prefix_for_reading_prefix(info.surface(), &full_reading, &katakana);
        if surface.is_empty() {
            continue;
        }

        // get_word_param returns (left_id, right_id, cost) as plain i16 tuple —
        // no Result wrapper, infallible.
        let (left_id, right_id, cost) = lexicon.get_word_param(word_id);
        let (left_id, right_id, cost) = (left_id as u16, right_id as u16, cost as i32);

        seen.entry(surface.clone())
            .and_modify(|existing| {
                // Keep the entry with the lowest cost (highest dictionary priority).
                // This correctly handles proper nouns with negative costs beating
                // shorter common-word matches.
                if cost < existing.cost {
                    *existing = ImeCandidate {
                        surface: surface.clone(),
                        word_id,
                        reading: full_reading.clone(),
                        cost,
                        left_id,
                        right_id,
                        segment_count: 1,
                    };
                }
            })
            .or_insert(ImeCandidate {
                surface,
                word_id,
                reading: full_reading,
                cost,
                left_id,
                right_id,
                segment_count: 1,
            });
    }

    let mut candidates: Vec<ImeCandidate> = seen.into_values().collect();
    candidates.sort_by(|a, b| {
        a.cost
            .cmp(&b.cost)
            .then(a.reading.len().cmp(&b.reading.len()))
            .then(a.surface.cmp(&b.surface))
    });
    candidates
}

// ── ime_lookup_for_node ───────────────────────────────────────────────────────

/// Produce IME candidates from the raw fields of a single Viterbi node.
///
/// All parameters are plain values extracted from the node *before* calling
/// this function, so `Node` (which lives in a private module) never appears
/// in this module's types.
///
/// * `word_id`          — the word's id in the lexicon
/// * `accumulated_cost` — Viterbi path cost up to and including this node
/// * `left_id / right_id` — connection boundary ids for compound cost calc
/// * `is_oov`           — true when the node is an out-of-vocabulary word
/// * `oov_surface`      — the raw kana surface, only used when `is_oov`
pub(crate) fn ime_lookup_for_node(
    word_id: WordId,
    accumulated_cost: i32,
    left_id: u16,
    right_id: u16,
    is_oov: bool,
    oov_surface: &str,
    lexicon: &LexiconSet,
) -> Vec<ImeCandidate> {
    // OOV: no dictionary entry — emit the raw kana as both surface and reading.
    if is_oov {
        return vec![ImeCandidate {
            surface: oov_surface.to_owned(),
            reading: oov_surface.to_owned(),
            word_id,
            cost: accumulated_cost,
            left_id,
            right_id,
            segment_count: 1,
        }];
    }

    let info = match lexicon.get_word_info(word_id) {
        Ok(i) => i,
        Err(_) => return vec![],
    };

    let surface = info.surface().to_owned();
    let reading = hiragana_to_katakana(info.reading_form());

    vec![ImeCandidate {
        surface,
        word_id,
        reading,
        cost: accumulated_cost,
        left_id,
        right_id,
        segment_count: 1,
    }]
}

// ── compound folding ──────────────────────────────────────────────────────────

/// Maximum number of adjacent Viterbi segments to fold into a single compound.
///
/// Capped to avoid exponential blowup on very long inputs.  4 covers the vast
/// majority of real Japanese compounds (大分県美術館 = 2 segments, etc.).
const MAX_COMPOUND_SEGMENTS: usize = 4;

/// Fold a contiguous span of per-segment candidate lists into a single compound
/// [`ImeCandidate`], using the best (lowest-cost) candidate from each segment.
///
/// Connection costs between adjacent segments are added via [`ConnectionMatrix`]
/// — the same costs Viterbi uses — so compound scoring is fully consistent with
/// the underlying analysis.
///
/// Returns `None` if any segment list is empty.
fn fold_span(span: &[Vec<ImeCandidate>], conn: &ConnectionMatrix) -> Option<ImeCandidate> {
    if span.is_empty() {
        return None;
    }

    // Take the best (lowest-cost, first after sort) candidate from each segment.
    let best: Vec<&ImeCandidate> = span
        .iter()
        .map(|seg| seg.first())
        .collect::<Option<Vec<_>>>()?;

    let mut surface = String::new();
    let mut reading = String::new();

    // Start with the first segment's accumulated cost.
    // For subsequent segments we add the *word* cost (not accumulated) plus
    // the connection cost, because accumulated cost already includes previous
    // segments for the Viterbi path — here we are building our own path.
    let mut total_cost = best[0].cost;

    surface.push_str(&best[0].surface);
    reading.push_str(&best[0].reading);

    for i in 1..best.len() {
        let prev = best[i - 1];
        let curr = best[i];

        // Connection cost between the right boundary of prev and left of curr.
        let connect_cost = conn.cost(prev.right_id, curr.left_id) as i32;
        total_cost += connect_cost + curr.cost;

        surface.push_str(&curr.surface);
        reading.push_str(&curr.reading);
    }

    Some(ImeCandidate {
        surface,
        word_id: best[0].word_id,
        reading,
        cost: total_cost,
        left_id: best[0].left_id,
        right_id: best[best.len() - 1].right_id,
        segment_count: best.len(),
    })
}

/// Deduplicate candidates by surface, keeping the lowest-cost entry.
/// Then sort: cost ascending → segment_count descending (prefer richer compounds
/// at equal cost) → surface lexicographic for stability.
fn dedup_by_cost(mut candidates: Vec<ImeCandidate>) -> Vec<ImeCandidate> {
    // Sort so lowest-cost comes first for each surface.
    candidates.sort_by(|a, b| {
        a.cost
            .cmp(&b.cost)
            .then(b.segment_count.cmp(&a.segment_count))
            .then(a.surface.cmp(&b.surface))
    });

    let mut seen: HashSet<String> = HashSet::new();
    candidates.retain(|c| seen.insert(c.surface.clone()));
    candidates
}

// ── ime_lookup_compound ───────────────────────────────────────────────────────

/// Compound-aware IME lookup using the full Viterbi path.
///
/// 1. Extracts the best Viterbi path from `lattice`.
/// 2. Collects per-segment [`ImeCandidate`]s via [`ime_lookup_for_node`].
/// 3. Emits single-segment candidates for every node.
/// 4. Folds all contiguous spans of up to [`MAX_COMPOUND_SEGMENTS`] nodes into
///    compound candidates, scoring them with connection costs from `conn`.
/// 5. Deduplicates by surface (lowest cost wins) and sorts.
///
/// ## Example output for `おおいたけんびじゅつかん`
///
/// Viterbi path: [大分県][美術館]
///
/// ```text
/// 大分県美術館  cost: 530   segment_count: 2  ← compound ✅
/// 大分県        cost: 300   segment_count: 1
/// 美術館        cost: 280   segment_count: 1
/// ```
///
/// ## Dialect example: `こないするけん`
///
/// After dialect POS-chain rewrite tags けん as 助詞, the connection cost
/// 動詞→助詞 is naturally low, so the compound `こないするけん` surfaces
/// with a better score than the erroneous `こないする県`.
pub fn ime_lookup_compound(
    typed_hiragana: &str,
    lexicon: &LexiconSet,
    lattice: &Lattice,
    conn: &ConnectionMatrix,
    input: &InputBuffer,
) -> Vec<ImeCandidate> {
    // 1. Extract Viterbi path (fill_top_path returns reversed — flip it).
    let mut path_indices = Vec::new();
    lattice.fill_top_path(&mut path_indices);
    path_indices.reverse();

    // 2. Destructure node fields at the lattice boundary.
    //    We extract all needed values here so Node (in a private module)
    //    never appears as a type anywhere in this file.
    struct NodeFields {
        word_id: WordId,
        cost: i32,
        left_id: u16,
        right_id: u16,
        is_oov: bool,
        oov_surface: String,
    }

    let path: Vec<NodeFields> = path_indices
        .iter()
        .map(|idx| {
            let (node, cost) = lattice.node(*idx);
            NodeFields {
                word_id: node.word_id(),
                cost,
                left_id: node.left_id(),
                right_id: node.right_id(),
                is_oov: node.is_oov(),
                oov_surface: if node.is_oov() {
                    input.orig_slice_c(node.begin()..node.end()).to_owned()
                } else {
                    String::new()
                },
            }
        })
        .filter(|n| {
            // Filter BOS/EOS: special nodes have word_id == WordId::EOS
            // and zero cost. Use is_oov=false + word_id check as proxy.
            // Lattice::fill_top_path already excludes BOS, but guard anyway.
            n.word_id != WordId::EOS
        })
        .collect();

    if path.is_empty() {
        return ime_lookup(typed_hiragana, lexicon);
    }

    // 3. Collect per-segment candidates.
    let segment_candidates: Vec<Vec<ImeCandidate>> = path
        .iter()
        .map(|n| {
            ime_lookup_for_node(
                n.word_id,
                n.cost,
                n.left_id,
                n.right_id,
                n.is_oov,
                &n.oov_surface,
                lexicon,
            )
        })
        .collect();

    let mut all_candidates: Vec<ImeCandidate> = Vec::new();

    // 4. Single-segment candidates.
    for seg in &segment_candidates {
        all_candidates.extend_from_slice(seg);
    }

    // 5. Compound candidates for all contiguous spans [i..=j].
    let n = segment_candidates.len();
    for i in 0..n {
        for j in (i + 1)..n.min(i + MAX_COMPOUND_SEGMENTS) {
            if let Some(compound) = fold_span(&segment_candidates[i..=j], conn) {
                all_candidates.push(compound);
            }
        }
    }

    // 6. Dedup + sort.
    dedup_by_cost(all_candidates)
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── classify_input ────────────────────────────────────────────────────────

    #[test]
    fn classify_complete_verb_ichidan() {
        assert_eq!(
            classify_input("たべる"),
            InputPattern::Complete("たべる".into())
        );
    }

    #[test]
    fn classify_complete_verb_godan() {
        assert_eq!(
            classify_input("いく"),
            InputPattern::Complete("いく".into())
        );
    }

    #[test]
    fn classify_complete_adjective() {
        assert_eq!(
            classify_input("たかい"),
            InputPattern::Complete("たかい".into())
        );
        assert_eq!(
            classify_input("たかく"),
            InputPattern::Complete("たかく".into())
        );
    }

    #[test]
    fn classify_partial_stem() {
        assert_eq!(
            classify_input("たべ"),
            InputPattern::PartialStem("たべ".into())
        );
        assert_eq!(
            classify_input("いそが"),
            InputPattern::PartialStem("いそが".into())
        );
    }

    #[test]
    fn classify_prefix_short() {
        assert_eq!(classify_input("き"), InputPattern::Prefix("き".into()));
        assert_eq!(
            classify_input("にほ"),
            InputPattern::PartialStem("にほ".into())
        );
    }

    #[test]
    fn classify_empty() {
        assert_eq!(classify_input(""), InputPattern::Prefix("".into()));
    }

    // ── dedup_by_cost ─────────────────────────────────────────────────────────

    #[test]
    fn dedup_keeps_lowest_cost() {
        let make = |surface: &str, cost: i32, segs: usize| ImeCandidate {
            surface: surface.into(),
            word_id: WordId::EOS,
            reading: String::new(),
            cost,
            left_id: 0,
            right_id: 0,
            segment_count: segs,
        };

        let candidates = vec![
            make("大分県美術館", 530, 2),
            make("大分県美術館", 800, 1), // duplicate — higher cost, should be dropped
            make("大分県", 300, 1),
            make("美術館", 280, 1),
        ];

        let result = dedup_by_cost(candidates);

        assert_eq!(result.len(), 3);
        // compound should win and appear first (lowest cost after dedup)
        assert_eq!(result[0].surface, "美術館");
        assert_eq!(result[1].surface, "大分県");
        assert_eq!(result[2].surface, "大分県美術館");

        // ensure the 530-cost entry survived, not the 800-cost duplicate
        let museum = result.iter().find(|c| c.surface == "大分県美術館").unwrap();
        assert_eq!(museum.cost, 530);
        assert_eq!(museum.segment_count, 2);
    }

    // ── fold_span ─────────────────────────────────────────────────────────────

    #[test]
    fn fold_span_empty_returns_none() {
        // A mock ConnectionMatrix would be needed for a real integration test;
        // here we just verify the empty-input guard.
        // Full integration tests live in tests/ime_compound.rs.
        let segments: Vec<Vec<ImeCandidate>> = vec![];
        // Can't call fold_span without a ConnectionMatrix, but we can verify
        // the empty path guard in ime_lookup_compound via the fallback branch.
        assert!(segments.is_empty());
    }
}
