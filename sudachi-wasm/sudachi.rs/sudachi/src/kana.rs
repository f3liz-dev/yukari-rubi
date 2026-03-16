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

//! Kana (hiragana/katakana) conversion utilities.
//!
//! These are primarily intended for IME use, where the user types hiragana
//! but dictionary readings are stored in katakana.

/// Hiragana codepoint range: U+3041 – U+3096
const HIRAGANA_START: u32 = 0x3041;
const HIRAGANA_END: u32 = 0x3096;
/// Katakana codepoint range: U+30A1 – U+30F6
const KATAKANA_START: u32 = 0x30A1;
const KATAKANA_END: u32 = 0x30F6;
/// Offset between matching hiragana and katakana codepoints
const KANA_OFFSET: u32 = KATAKANA_START - HIRAGANA_START; // 0x60

/// Returns `true` if `c` is a hiragana or katakana character.
pub fn is_kana(c: char) -> bool {
    let cp = c as u32;
    (cp >= HIRAGANA_START && cp <= HIRAGANA_END) || (cp >= KATAKANA_START && cp <= KATAKANA_END)
}

/// Convert a single character to its katakana equivalent.
///
/// If `c` is hiragana it is shifted to the corresponding katakana codepoint.
/// All other characters (kanji, ASCII, already-katakana, etc.) are returned
/// unchanged.
pub fn to_katakana_char(c: char) -> char {
    let cp = c as u32;
    if cp >= HIRAGANA_START && cp <= HIRAGANA_END {
        char::from_u32(cp + KANA_OFFSET).unwrap_or(c)
    } else {
        c
    }
}

/// Given a word's `surface`, its katakana `reading`, and a katakana `prefix`,
/// return the portion of `surface` whose reading corresponds to `prefix`.
///
/// This is the core surface-alignment function for IME candidate generation.
/// When the user has typed a reading prefix that is shorter than a matched
/// word's full reading, this function computes which part of the surface form
/// should be shown as the candidate.
///
/// The alignment uses an **okurigana heuristic**:
/// - Trailing kana characters in `surface` that correspond 1-to-1 with
///   trailing characters in `reading` are treated as okurigana and aligned
///   directly.
/// - The remaining (kanji) portion of the surface is divided proportionally
///   across the kanji reading segment using floor division.
///
/// # Examples
///
/// ```
/// use sudachi::kana::surface_prefix_for_reading_prefix;
/// // verb: 食べる / タベル, typed prefix タベ → shows 食べ
/// assert_eq!(surface_prefix_for_reading_prefix("食べる", "タベル", "タベ"), "食べ");
/// // verb: 行く / イク, typed prefix イ → shows 行
/// assert_eq!(surface_prefix_for_reading_prefix("行く", "イク", "イ"), "行");
/// // exact match: full prefix = full reading → return whole surface
/// assert_eq!(surface_prefix_for_reading_prefix("食べ", "タベ", "タベ"), "食べ");
/// // compound: 日本語 / ニホンゴ, prefix ニホン → shows 日本
/// assert_eq!(surface_prefix_for_reading_prefix("日本語", "ニホンゴ", "ニホン"), "日本");
/// ```
pub fn surface_prefix_for_reading_prefix(surface: &str, reading: &str, prefix: &str) -> String {
    let s_chars: Vec<char> = surface.chars().collect();
    let r_chars: Vec<char> = reading.chars().collect();
    let p_len = prefix.chars().count();
    let r_len = r_chars.len();

    if p_len == 0 {
        return String::new();
    }
    if p_len >= r_len || s_chars.is_empty() {
        return surface.to_owned();
    }

    // Identify trailing okurigana: scan from the right, matching surface kana
    // chars against reading kana chars one-to-one.
    let mut oki = 0usize;
    let mut si = s_chars.len();
    let mut ri = r_len;
    while si > 0 && ri > 0 {
        let sc = s_chars[si - 1];
        let rc = r_chars[ri - 1];
        if to_katakana_char(sc) == rc {
            oki += 1;
            si -= 1;
            ri -= 1;
        } else {
            break;
        }
    }

    // si = number of non-okurigana (kanji) surface chars
    // ri = length of kanji reading (reading chars before okurigana)
    let kanji_s = si;
    let kanji_r = ri;

    if p_len <= kanji_r {
        // Prefix falls entirely within the kanji reading segment.
        // Distribute proportionally: floor(p_len * kanji_s / kanji_r), min 1.
        let include = if kanji_r == 0 {
            0
        } else {
            ((p_len * kanji_s) / kanji_r).max(if kanji_s > 0 { 1 } else { 0 })
        };
        s_chars[..include.min(kanji_s)].iter().collect()
    } else {
        // Prefix extends into okurigana: include all kanji chars plus the
        // appropriate number of okurigana chars.
        let oki_included = (p_len - kanji_r).min(oki);
        let total = kanji_s + oki_included;
        s_chars[..total].iter().collect()
    }
}

/// Convert hiragana characters to their katakana equivalents.
///
/// Non-hiragana characters (including existing katakana, kanji, ASCII, etc.)
/// are passed through unchanged.
///
/// # Examples
///
/// ```
/// use sudachi::kana::hiragana_to_katakana;
/// assert_eq!(hiragana_to_katakana("たべる"), "タベル");
/// assert_eq!(hiragana_to_katakana("ABC"), "ABC");
/// assert_eq!(hiragana_to_katakana("わたしは"), "ワタシハ");
/// ```
pub fn hiragana_to_katakana(s: &str) -> String {
    s.chars()
        .map(|c| {
            let cp = c as u32;
            if cp >= HIRAGANA_START && cp <= HIRAGANA_END {
                // Safety: offset stays in valid Unicode scalar range
                char::from_u32(cp + KANA_OFFSET).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

/// Convert katakana characters to their hiragana equivalents.
///
/// Non-katakana characters (including existing hiragana, kanji, ASCII, etc.)
/// are passed through unchanged.
///
/// # Examples
///
/// ```
/// use sudachi::kana::katakana_to_hiragana;
/// assert_eq!(katakana_to_hiragana("タベル"), "たべる");
/// assert_eq!(katakana_to_hiragana("ABC"), "ABC");
/// assert_eq!(katakana_to_hiragana("ワタシハ"), "わたしは");
/// ```
pub fn katakana_to_hiragana(s: &str) -> String {
    s.chars()
        .map(|c| {
            let cp = c as u32;
            if cp >= KATAKANA_START && cp <= KATAKANA_END {
                char::from_u32(cp - KANA_OFFSET).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hiragana_to_katakana_basic() {
        assert_eq!(hiragana_to_katakana("たべる"), "タベル");
        assert_eq!(hiragana_to_katakana("わたしは"), "ワタシハ");
        assert_eq!(hiragana_to_katakana("がくせい"), "ガクセイ");
    }

    #[test]
    fn hiragana_to_katakana_passthrough() {
        assert_eq!(hiragana_to_katakana("ABC"), "ABC");
        assert_eq!(hiragana_to_katakana("食べる"), "食ベル"); // kanji unchanged, hiragana converted
        assert_eq!(hiragana_to_katakana("タベル"), "タベル"); // already katakana, unchanged
    }

    #[test]
    fn katakana_to_hiragana_basic() {
        assert_eq!(katakana_to_hiragana("タベル"), "たべる");
        assert_eq!(katakana_to_hiragana("ワタシハ"), "わたしは");
        assert_eq!(katakana_to_hiragana("ガクセイ"), "がくせい");
    }

    #[test]
    fn katakana_to_hiragana_passthrough() {
        assert_eq!(katakana_to_hiragana("ABC"), "ABC");
        assert_eq!(katakana_to_hiragana("食べる"), "食べる");
        assert_eq!(katakana_to_hiragana("たべる"), "たべる"); // already hiragana
    }

    #[test]
    fn round_trip() {
        let original = "たべる";
        assert_eq!(katakana_to_hiragana(&hiragana_to_katakana(original)), original);

        let kata = "タベル";
        assert_eq!(hiragana_to_katakana(&katakana_to_hiragana(kata)), kata);
    }

    #[test]
    fn boundary_chars() {
        // ぁ (U+3041) and ゖ (U+3096) are the boundary hiragana chars
        assert_eq!(hiragana_to_katakana("ぁ"), "ァ");
        assert_eq!(hiragana_to_katakana("ゖ"), "ヶ");
    }

    #[test]
    fn is_kana_basic() {
        assert!(is_kana('あ'));
        assert!(is_kana('ア'));
        assert!(is_kana('る'));
        assert!(is_kana('ル'));
        assert!(!is_kana('食'));
        assert!(!is_kana('A'));
        assert!(!is_kana(' '));
    }

    #[test]
    fn to_katakana_char_converts() {
        assert_eq!(to_katakana_char('た'), 'タ');
        assert_eq!(to_katakana_char('べ'), 'ベ');
        assert_eq!(to_katakana_char('る'), 'ル');
        // non-hiragana unchanged
        assert_eq!(to_katakana_char('食'), '食');
        assert_eq!(to_katakana_char('ア'), 'ア');
    }

    // surface_prefix_for_reading_prefix tests

    #[test]
    fn surface_prefix_exact_match() {
        // full prefix == full reading → return whole surface
        assert_eq!(surface_prefix_for_reading_prefix("食べ", "タベ", "タベ"), "食べ");
        assert_eq!(surface_prefix_for_reading_prefix("行く", "イク", "イク"), "行く");
    }

    #[test]
    fn surface_prefix_longer_prefix_returns_surface() {
        // prefix >= reading length → return whole surface
        assert_eq!(
            surface_prefix_for_reading_prefix("食べる", "タベル", "タベルナ"),
            "食べる"
        );
    }

    #[test]
    fn surface_prefix_verb_ichidan() {
        // 食べる / タベル, prefix タベ → 食べ
        assert_eq!(
            surface_prefix_for_reading_prefix("食べる", "タベル", "タベ"),
            "食べ"
        );
    }

    #[test]
    fn surface_prefix_verb_godan_ku() {
        // 行く / イク, prefix イ → 行
        assert_eq!(surface_prefix_for_reading_prefix("行く", "イク", "イ"), "行");
    }

    #[test]
    fn surface_prefix_adjective() {
        // 高い / タカイ, prefix タカ → 高 (kanji portion)
        assert_eq!(
            surface_prefix_for_reading_prefix("高い", "タカイ", "タカ"),
            "高"
        );
        // prefix タ → 高 (min 1 kanji)
        assert_eq!(surface_prefix_for_reading_prefix("高い", "タカイ", "タ"), "高");
    }

    #[test]
    fn surface_prefix_compound_kanji() {
        // 日本語 / ニホンゴ, prefix ニホン → 日本
        assert_eq!(
            surface_prefix_for_reading_prefix("日本語", "ニホンゴ", "ニホン"),
            "日本"
        );
        // 東京都 / トウキョウト, prefix トウキョウ → 東京
        assert_eq!(
            surface_prefix_for_reading_prefix("東京都", "トウキョウト", "トウキョウ"),
            "東京"
        );
    }

    #[test]
    fn surface_prefix_empty_prefix() {
        assert_eq!(
            surface_prefix_for_reading_prefix("食べる", "タベル", ""),
            ""
        );
    }

    #[test]
    fn surface_prefix_okurigana_partial() {
        // 食べない / タベナイ, prefix タベナ → 食べな
        assert_eq!(
            surface_prefix_for_reading_prefix("食べない", "タベナイ", "タベナ"),
            "食べな"
        );
    }
}
