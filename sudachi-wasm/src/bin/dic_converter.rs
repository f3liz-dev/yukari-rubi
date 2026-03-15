//! Dictionary converter: YADA → MARISA with zstd compression.
//!
//! Reads a standard Sudachi `.dic` file and writes a new `.dic` file with:
//! 1. MARISA trie index (replacing YADA double-array trie)
//! 2. Block-compressed connection matrix (zstd, 256×256 blocks)
//! 3. Block-compressed word_infos (zstd, 128-record blocks)
//!
//! Usage:
//!   dic_converter <input.dic> <output.dic>

use rsmarisa::grimoire::io::Writer;
use rsmarisa::{Agent, Keyset};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader};
use std::process;

#[cfg(not(target_arch = "wasm32"))]
use zstd::bulk;

const HEADER_SIZE: usize = 272;
const BLOCK_SIZE: usize = 256;
const COMPRESSED_MAGIC: u32 = 0x4D43_5A42; // "MCZB" — zstd-compressed blocks

/// Convert katakana to hiragana (reverse of `hiragana_to_katakana`).
///
/// Maps U+30A1..U+30F6 (ァ..ヶ) → U+3041..U+3096 (ぁ..ゖ) by subtracting 0x60.
/// All other characters are passed through unchanged.
fn katakana_to_hiragana(s: &str) -> String {
    s.chars()
        .map(|c| {
            let cp = c as u32;
            if (0x30A1..=0x30F6).contains(&cp) {
                char::from_u32(cp - 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

const SYSTEM_DICT_V1: u64 = 0x7366d3f18bd111e7;
const SYSTEM_DICT_V2: u64 = 0xce9f011a92394434;
const USER_DICT_V2: u64 = 0x9fdeb5a90168d868;
const USER_DICT_V3: u64 = 0xca9811756ff64fb0;

fn read_u16_le(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}

fn read_i16_le(buf: &[u8], off: usize) -> i16 {
    i16::from_le_bytes(buf[off..off + 2].try_into().unwrap())
}

fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

fn has_grammar(version: u64) -> bool {
    matches!(
        version,
        SYSTEM_DICT_V1 | SYSTEM_DICT_V2 | USER_DICT_V2 | USER_DICT_V3
    )
}

// ── Grammar section parsing ─────────────────────────────────────────────

/// Parse grammar section, returning (pos_list_bytes, left_id, right_id, matrix_start, matrix_end).
fn parse_grammar(buf: &[u8], grammar_offset: usize) -> (Vec<u8>, usize, usize, usize, usize) {
    let mut off = grammar_offset;
    let pos_count = read_u16_le(buf, off) as usize;
    off += 2;

    for _ in 0..pos_count {
        for _ in 0..6 {
            let str_len = buf[off] as usize;
            if str_len >= 128 {
                let actual = ((str_len & 0x7f) << 8) | buf[off + 1] as usize;
                off += 2 + actual * 2;
            } else {
                off += 1 + str_len * 2;
            }
        }
    }

    let left_id = read_i16_le(buf, off) as usize;
    off += 2;
    let right_id = read_i16_le(buf, off) as usize;
    off += 2;

    let matrix_start = off;
    let matrix_end = off + left_id * right_id * 2;

    // pos_list_bytes includes the pos_count, POS entries, left_id, right_id
    let pos_list_bytes = buf[grammar_offset..matrix_start].to_vec();

    (pos_list_bytes, left_id, right_id, matrix_start, matrix_end)
}

fn train_zstd_dictionary(samples: &[Vec<u8>], label: &str) -> Vec<u8> {
    let sample_sizes: Vec<usize> = samples.iter().map(|s| s.len()).collect();
    let continuous: Vec<u8> = samples.iter().flat_map(|s| s.iter().copied()).collect();
    match zstd::dict::from_continuous(&continuous, &sample_sizes, 256 * 1024) {
        Ok(dict) => {
            eprintln!("    Trained {} dictionary: {} bytes", label, dict.len());
            dict
        }
        Err(err) => {
            eprintln!("    Skipping {} dictionary training: {}", label, err);
            Vec::new()
        }
    }
}

fn compress_zstd_samples(samples: &[Vec<u8>], dict: &[u8]) -> Vec<Vec<u8>> {
    if dict.is_empty() {
        let mut compressor = bulk::Compressor::new(19).expect("failed to create zstd compressor");
        samples
            .iter()
            .map(|sample| compressor.compress(sample).expect("zstd compression failed"))
            .collect()
    } else {
        let mut compressor = bulk::Compressor::with_dictionary(19, dict)
            .expect("failed to create compressor with dictionary");
        samples
            .iter()
            .map(|sample| compressor.compress(sample).expect("zstd compression failed"))
            .collect()
    }
}

/// Compress the connection matrix into 256×256 zstd blocks.
///
/// Raw i16 matrix cells are grouped into blocks, a zstd dictionary is trained
/// from the full set of blocks, and then each block is compressed with that
/// dictionary. Unlike the removed MCZD format, this stores plain values rather
/// than row deltas.
///
/// Output format:
/// ```text
/// [COMPRESSED_MAGIC: u32]
/// [num_left: u16][num_right: u16]
/// [num_blocks: u32]
/// [dict_size: u32]
/// [dictionary: u8 × dict_size]
/// [block_index: (offset: u32, compressed_size: u32) × num_blocks]
/// [compressed_block_data: ...]
/// ```
fn compress_connection_matrix(
    buf: &[u8],
    matrix_start: usize,
    num_left: usize,
    num_right: usize,
) -> Vec<u8> {
    let num_row_blocks = (num_right + BLOCK_SIZE - 1) / BLOCK_SIZE;
    let num_col_blocks = (num_left + BLOCK_SIZE - 1) / BLOCK_SIZE;
    let num_blocks = num_row_blocks * num_col_blocks;

    let mut samples: Vec<Vec<u8>> = Vec::with_capacity(num_blocks);
    for rb in 0..num_row_blocks {
        for cb in 0..num_col_blocks {
            let mut block_data = Vec::new();
            for r in (rb * BLOCK_SIZE)..std::cmp::min((rb + 1) * BLOCK_SIZE, num_right) {
                for c in (cb * BLOCK_SIZE)..std::cmp::min((cb + 1) * BLOCK_SIZE, num_left) {
                    let idx = r * num_left + c;
                    let pos = matrix_start + idx * 2;
                    block_data.extend_from_slice(&buf[pos..pos + 2]);
                }
            }
            samples.push(block_data);
        }
    }

    let dict = train_zstd_dictionary(&samples, "connection matrix");
    let compressed_blocks = compress_zstd_samples(&samples, &dict);

    let mut result = Vec::new();
    result.extend_from_slice(&COMPRESSED_MAGIC.to_le_bytes());
    result.extend_from_slice(&(num_left as u16).to_le_bytes());
    result.extend_from_slice(&(num_right as u16).to_le_bytes());
    result.extend_from_slice(&(num_blocks as u32).to_le_bytes());
    result.extend_from_slice(&(dict.len() as u32).to_le_bytes());
    result.extend_from_slice(&dict);

    let mut data_offset: u32 = 0;
    for block in &compressed_blocks {
        result.extend_from_slice(&data_offset.to_le_bytes());
        result.extend_from_slice(&(block.len() as u32).to_le_bytes());
        data_offset += block.len() as u32;
    }

    for block in &compressed_blocks {
        result.extend_from_slice(block);
    }

    result
}

// ── YADA trie helpers ───────────────────────────────────────────────────

#[inline]
fn yada_label(unit: u32) -> u32 {
    unit & ((1 << 31) | 0xFF)
}

#[inline]
fn yada_has_leaf(unit: u32) -> bool {
    ((unit >> 8) & 1) == 1
}

#[inline]
fn yada_offset(unit: u32) -> u32 {
    (unit >> 10) << ((unit & (1 << 9)) >> 6)
}

#[inline]
fn yada_value(unit: u32) -> u32 {
    unit & ((1 << 31) - 1)
}

fn parse_yada_array(buf: &[u8], offset: usize) -> (Vec<u32>, usize) {
    let trie_size = read_u32_le(buf, offset) as usize;
    let data_start = offset + 4;
    let data_end = data_start + trie_size * 4;
    let mut array = Vec::with_capacity(trie_size);
    for i in 0..trie_size {
        array.push(read_u32_le(buf, data_start + i * 4));
    }
    (array, data_end)
}

fn extract_yada_entries(array: &[u32]) -> Vec<(Vec<u8>, u32)> {
    let mut results = Vec::new();
    if array.is_empty() {
        return results;
    }
    let root_unit = array[0];
    let root_offset = yada_offset(root_unit) as usize;
    let mut path = Vec::new();
    yada_dfs(array, root_offset, &mut path, &mut results);
    results
}

fn yada_dfs(array: &[u32], node_pos: usize, path: &mut Vec<u8>, results: &mut Vec<(Vec<u8>, u32)>) {
    for k in 0u16..=255 {
        let child_pos = node_pos ^ (k as usize);
        if child_pos >= array.len() {
            continue;
        }
        let unit = array[child_pos];
        if yada_label(unit) != k as u32 {
            continue;
        }
        path.push(k as u8);
        let new_node_pos = child_pos ^ yada_offset(unit) as usize;
        if yada_has_leaf(unit) && new_node_pos < array.len() {
            let val = yada_value(array[new_node_pos]);
            results.push((path.clone(), val));
        }
        yada_dfs(array, new_node_pos, path, results);
        path.pop();
    }
}

fn build_marisa_section(entries: &[(Vec<u8>, u32)]) -> Vec<u8> {
    let mut keyset = Keyset::new();
    for (key, _) in entries {
        keyset
            .push_back_bytes(key, 1.0)
            .expect("failed to add key to keyset");
    }
    let mut trie = rsmarisa::Trie::new();
    trie.build(&mut keyset, 0);

    let num_keys = trie.num_keys();
    let mut id_to_offset = vec![0u32; num_keys];
    let mut agent = Agent::new();
    for (key, offset_val) in entries {
        agent.set_query_bytes(key);
        assert!(trie.lookup(&mut agent), "key not found after build");
        id_to_offset[agent.key().id()] = *offset_val;
    }

    let mut writer = Writer::from_vec(Vec::new());
    trie.write(&mut writer).expect("failed to serialize trie");
    let trie_bytes = writer.into_inner().expect("failed to get trie bytes");

    let mut result = Vec::with_capacity(4 + trie_bytes.len() + 4 + id_to_offset.len() * 4);
    result.extend_from_slice(&(trie_bytes.len() as u32).to_le_bytes());
    result.extend_from_slice(&trie_bytes);
    result.extend_from_slice(&(id_to_offset.len() as u32).to_le_bytes());
    for &off in &id_to_offset {
        result.extend_from_slice(&off.to_le_bytes());
    }
    result
}

// ── Word infos block compression ────────────────────────────────────────

const BLOCK_WI_MAGIC: u32 = 0x4D57_4942; // "MWIB"
const WI_BLOCK_SIZE: usize = 128;

/// Block-compress word_info records with zstd.
///
/// Groups consecutive records into independently compressed blocks and
/// stores per-record intra-block offsets for direct lookup.
///
/// Output format:
/// ```text
/// [MAGIC: u32]
/// [num_words: u32]
/// [records_per_block: u16]
/// [num_blocks: u32]
/// [dict_size: u32]
/// [dictionary: u8 × dict_size]
/// [record_offsets: u32 × num_words]        — byte offset within decompressed block
/// [block_index: (offset: u32, size: u32) × num_blocks]
/// [compressed_block_data: ...]
/// ```
fn block_compress_word_infos(buf: &[u8], word_infos_start: usize, num_words: usize) -> Vec<u8> {
    let records_per_block = WI_BLOCK_SIZE;
    let num_blocks = num_words.div_ceil(records_per_block);

    let mut orig_offsets = Vec::with_capacity(num_words);
    for i in 0..num_words {
        orig_offsets.push(read_u32_le(buf, word_infos_start + i * 4) as usize);
    }

    let data_end = buf.len();
    let mut record_offsets = vec![0u32; num_words];
    let mut samples: Vec<Vec<u8>> = Vec::with_capacity(num_blocks);

    for block_idx in 0..num_blocks {
        let start_word = block_idx * records_per_block;
        let end_word = std::cmp::min(start_word + records_per_block, num_words);

        let block_data_start = orig_offsets[start_word];
        let block_data_end = if end_word < num_words {
            orig_offsets[end_word]
        } else {
            data_end
        };

        let block_data = &buf[block_data_start..block_data_end];

        for i in start_word..end_word {
            record_offsets[i] = (orig_offsets[i] - block_data_start) as u32;
        }

        samples.push(block_data.to_vec());
    }

    let dict = train_zstd_dictionary(&samples, "word_infos");
    let compressed_blocks = compress_zstd_samples(&samples, &dict);

    let mut result = Vec::new();
    result.extend_from_slice(&BLOCK_WI_MAGIC.to_le_bytes());
    result.extend_from_slice(&(num_words as u32).to_le_bytes());
    result.extend_from_slice(&(records_per_block as u16).to_le_bytes());
    result.extend_from_slice(&(num_blocks as u32).to_le_bytes());
    result.extend_from_slice(&(dict.len() as u32).to_le_bytes());
    result.extend_from_slice(&dict);

    for &off in &record_offsets {
        result.extend_from_slice(&off.to_le_bytes());
    }

    let mut data_offset: u32 = 0;
    for block in &compressed_blocks {
        result.extend_from_slice(&data_offset.to_le_bytes());
        result.extend_from_slice(&(block.len() as u32).to_le_bytes());
        data_offset += block.len() as u32;
    }

    for block in &compressed_blocks {
        result.extend_from_slice(block);
    }

    result
}

/// Build a reading-keyed MARISA trie from the output dictionary.
///
/// Parses the lexicon section of the output dictionary to iterate all words,
/// collects reading → word_ids mapping, then builds a reading trie section
/// with RTRI magic.
fn build_reading_trie_section(
    dict_bytes: &[u8],
    lexicon_offset: usize,
    has_synonym_group_ids: bool,
) -> Vec<u8> {
    use std::collections::BTreeMap;
    use sudachi::dic::lexicon::Lexicon;
    use sudachi::kana::hiragana_to_katakana;

    const READING_TRIE_MAGIC: u32 = 0x5254_5249; // "RTRI"

    // Parse lexicon directly from the MARISA-format output
    let lexicon = Lexicon::parse(dict_bytes, lexicon_offset, has_synonym_group_ids)
        .expect("Failed to parse lexicon for reading trie building");

    // Collect reading → [raw_word_ids]
    let mut reading_to_ids: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (word_id, result) in lexicon.iter_reading_words() {
        if let Ok(info) = result {
            let reading = hiragana_to_katakana(info.reading_form());
            if !reading.is_empty() {
                reading_to_ids
                    .entry(reading)
                    .or_default()
                    .push(word_id.word());
            }
        }
    }

    eprintln!("  Reading trie: {} unique readings", reading_to_ids.len());

    // Build the word_id table: for each reading, store [count: u8][ids: u32 × count]
    let mut word_id_table = Vec::new();
    let mut entries: Vec<(Vec<u8>, u32)> = Vec::new();

    for (reading, ids) in &reading_to_ids {
        let offset = word_id_table.len() as u32;
        let count = ids.len().min(255) as u8;
        word_id_table.push(count);
        for &wid in ids.iter().take(255) {
            word_id_table.extend_from_slice(&wid.to_le_bytes());
        }
        entries.push((reading.as_bytes().to_vec(), offset));
    }

    // Build the MARISA trie from reading strings
    let mut keyset = Keyset::new();
    for (key, _) in &entries {
        keyset
            .push_back_bytes(key, 1.0)
            .expect("failed to add reading to keyset");
    }
    let mut trie = rsmarisa::Trie::new();
    trie.build(&mut keyset, 0);

    let num_keys = trie.num_keys();
    let mut id_to_offset = vec![0u32; num_keys];
    let mut agent = Agent::new();
    for (key, offset_val) in &entries {
        agent.set_query_bytes(key);
        assert!(trie.lookup(&mut agent), "reading key not found after build");
        id_to_offset[agent.key().id()] = *offset_val;
    }

    let mut writer = Writer::from_vec(Vec::new());
    trie.write(&mut writer)
        .expect("failed to serialize reading trie");
    let trie_bytes = writer
        .into_inner()
        .expect("failed to get reading trie bytes");

    // Assemble RTRI section
    let mut result = Vec::new();
    // Magic
    result.extend_from_slice(&READING_TRIE_MAGIC.to_le_bytes());
    // Trie data
    result.extend_from_slice(&(trie_bytes.len() as u32).to_le_bytes());
    result.extend_from_slice(&trie_bytes);
    // ID-to-offset map
    result.extend_from_slice(&(id_to_offset.len() as u32).to_le_bytes());
    for &off in &id_to_offset {
        result.extend_from_slice(&off.to_le_bytes());
    }
    // Word ID table
    result.extend_from_slice(&(word_id_table.len() as u32).to_le_bytes());
    result.extend_from_slice(&word_id_table);

    eprintln!(
        "  Reading trie section: {} bytes ({} keys)",
        result.len(),
        num_keys
    );
    result
}

/// Build reading-keyed MARISA trie + word_id table to replace the surface-keyed
/// main lexicon sections.
///
/// This makes Viterbi operate on reading (hiragana) keys so that kanji candidates
/// are selected during lattice search with full connection-cost awareness, rather
/// than being applied in post-processing.
///
/// Returns `(marisa_trie_section, word_id_table_section)` where each section
/// includes its size prefix, matching the format expected by `Lexicon::parse`.
fn build_reading_keyed_sections(
    dict_bytes: &[u8],
    lexicon_offset: usize,
    has_synonym_group_ids: bool,
) -> (Vec<u8>, Vec<u8>) {
    use std::collections::BTreeMap;
    use sudachi::dic::lexicon::Lexicon;

    let lexicon = Lexicon::parse(dict_bytes, lexicon_offset, has_synonym_group_ids)
        .expect("Failed to parse lexicon for reading-keyed rebuild");

    // Collect hiragana reading → [raw_word_ids]
    let mut reading_to_ids: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (word_id, result) in lexicon.iter_reading_words() {
        if let Ok(info) = result {
            let reading_hira = katakana_to_hiragana(info.reading_form());
            if !reading_hira.is_empty() {
                reading_to_ids
                    .entry(reading_hira)
                    .or_default()
                    .push(word_id.word());
            }
        }
    }

    eprintln!(
        "  Reading-keyed trie: {} unique readings",
        reading_to_ids.len()
    );

    // Build word_id table: [count: u8][word_ids: u32 × count] per reading
    let mut word_id_table_data = Vec::new();
    let mut entries: Vec<(Vec<u8>, u32)> = Vec::new();

    for (reading, ids) in &reading_to_ids {
        let offset = word_id_table_data.len() as u32;
        let count = ids.len().min(255) as u8;
        word_id_table_data.push(count);
        for &wid in ids.iter().take(255) {
            word_id_table_data.extend_from_slice(&wid.to_le_bytes());
        }
        entries.push((reading.as_bytes().to_vec(), offset));
    }

    // Build MARISA trie (reuses existing build_marisa_section)
    let marisa_section = build_marisa_section(&entries);

    // Format word_id table with u32 size prefix (matching Lexicon::parse expectation)
    let mut wit_section = Vec::with_capacity(4 + word_id_table_data.len());
    wit_section.extend_from_slice(&(word_id_table_data.len() as u32).to_le_bytes());
    wit_section.extend_from_slice(&word_id_table_data);

    eprintln!(
        "  Reading-keyed MARISA: {} bytes, word_id_table: {} bytes",
        marisa_section.len(),
        wit_section.len()
    );

    (marisa_section, wit_section)
}

/// Load adjusted costs from a Julia-processed CSV.
///
/// Returns a map from word_id → adjusted cost (i16).
/// CSV format: word_id,reading_hiragana,reading_katakana,surface,cost,...
fn load_cost_csv(path: &str) -> HashMap<u32, i16> {
    let file = fs::File::open(path).unwrap_or_else(|e| {
        eprintln!("Failed to open cost CSV {}: {}", path, e);
        process::exit(1);
    });
    let reader = BufReader::new(file);
    let mut costs = HashMap::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.unwrap();
        if line_num == 0 {
            continue; // skip header
        }
        // Parse: word_id,reading_h,reading_k,surface,cost,...
        // word_id is field 0, cost is field 4
        let fields: Vec<&str> = line.splitn(6, ',').collect();
        if fields.len() < 5 {
            continue;
        }
        if let (Ok(word_id), Ok(cost)) = (fields[0].parse::<u32>(), fields[4].parse::<i16>()) {
            costs.insert(word_id, cost);
        }
    }

    eprintln!("  Loaded {} cost adjustments from {}", costs.len(), path);
    costs
}

// ── Matrix patch loading ────────────────────────────────────────────────

/// Load matrix patches from a Julia-generated CSV.
///
/// Returns a map from (left_id, right_id) → delta (i16).
/// CSV format: left_id,right_id,delta
fn load_matrix_patches(path: &str) -> HashMap<(u16, u16), i16> {
    let file = fs::File::open(path).unwrap_or_else(|e| {
        eprintln!("Failed to open matrix patches {}: {}", path, e);
        process::exit(1);
    });
    let reader = BufReader::new(file);
    let mut patches = HashMap::new();

    for (line_num, line) in reader.lines().enumerate() {
        let line = line.unwrap();
        if line_num == 0 {
            continue; // skip header
        }
        let fields: Vec<&str> = line.splitn(4, ',').collect();
        if fields.len() < 3 {
            continue;
        }
        if let (Ok(left_id), Ok(right_id), Ok(delta)) = (
            fields[0].parse::<u16>(),
            fields[1].parse::<u16>(),
            fields[2].trim().parse::<i16>(),
        ) {
            patches.insert((left_id, right_id), delta);
        }
    }

    eprintln!("  Loaded {} matrix patches from {}", patches.len(), path);
    patches
}

/// Apply matrix patches to the raw connection matrix buffer in-place.
///
/// The matrix is stored as i16 values at `matrix_start`, indexed as
/// `matrix[right * num_left + left]`.
fn apply_matrix_patches(
    buf: &mut [u8],
    matrix_start: usize,
    num_left: usize,
    num_right: usize,
    patches: &HashMap<(u16, u16), i16>,
) -> u32 {
    let mut applied = 0u32;
    for (&(left_id, right_id), &delta) in patches {
        let li = left_id as usize;
        let ri = right_id as usize;
        if li >= num_left || ri >= num_right {
            continue;
        }
        let idx = ri * num_left + li;
        let pos = matrix_start + idx * 2;
        let current = i16::from_le_bytes(buf[pos..pos + 2].try_into().unwrap());
        let new_cost =
            (current as i32 + delta as i32).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        buf[pos..pos + 2].copy_from_slice(&new_cost.to_le_bytes());
        applied += 1;
    }
    applied
}

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse arguments: <input.dic> <output.dic> [--cost-csv <adjusted.csv>] [--matrix-patches <patches.csv>]
    if args.len() < 3 {
        eprintln!("Usage: {} <input.dic> <output.dic> [--cost-csv <adjusted.csv>] [--matrix-patches <patches.csv>] [--reading-keyed]", args[0]);
        eprintln!("Converts a YADA-format Sudachi dictionary to MARISA-format");
        eprintln!("with a zstd-compressed connection matrix and zstd-compressed word infos.");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --cost-csv <path>        Apply Julia-optimized costs from CSV");
        eprintln!("  --matrix-patches <path>  Apply connection matrix patches from CSV");
        eprintln!(
            "  --reading-keyed          Swap main trie to reading-keyed (hiragana) for KKC use"
        );
        process::exit(1);
    }

    let input_path = &args[1];
    let output_path = &args[2];

    // Parse optional flags
    let mut cost_csv_path: Option<String> = None;
    let mut matrix_patches_path: Option<String> = None;
    let mut reading_keyed = false;
    let mut i = 3;
    while i < args.len() {
        match args[i].as_str() {
            "--cost-csv" if i + 1 < args.len() => {
                cost_csv_path = Some(args[i + 1].clone());
                i += 2;
            }
            "--matrix-patches" if i + 1 < args.len() => {
                matrix_patches_path = Some(args[i + 1].clone());
                i += 2;
            }
            "--reading-keyed" => {
                reading_keyed = true;
                i += 1;
            }
            _ => {
                eprintln!("Unknown argument: {}", args[i]);
                process::exit(1);
            }
        }
    }

    let cost_adjustments: Option<HashMap<u32, i16>> = cost_csv_path.map(|p| load_cost_csv(&p));
    let matrix_patches: Option<HashMap<(u16, u16), i16>> =
        matrix_patches_path.map(|p| load_matrix_patches(&p));

    eprintln!("Reading {}...", input_path);
    let mut buf = fs::read(input_path).unwrap_or_else(|e| {
        eprintln!("Failed to read {}: {}", input_path, e);
        process::exit(1);
    });

    if buf.len() < HEADER_SIZE {
        eprintln!("File too small to be a Sudachi dictionary");
        process::exit(1);
    }

    let version = read_u64_le(&buf, 0);
    let has_gram = has_grammar(version);
    eprintln!(
        "  Header version: 0x{:016x}, has_grammar: {}",
        version, has_gram
    );

    // ── 1. Header ──────────────────────────────────────────────────────
    let mut output = Vec::with_capacity(buf.len());
    output.extend_from_slice(&buf[..HEADER_SIZE]);

    // ── 2. Grammar (POS list + compressed connection matrix) ────────
    let lexicon_offset;
    if has_gram {
        let (pos_list_bytes, left_id, right_id, matrix_start, matrix_end) =
            parse_grammar(&buf, HEADER_SIZE);

        // Write POS list + left_id + right_id
        output.extend_from_slice(&pos_list_bytes);

        // Apply matrix patches before compression
        if let Some(ref patches) = matrix_patches {
            let applied = apply_matrix_patches(&mut buf, matrix_start, left_id, right_id, patches);
            eprintln!(
                "  Matrix patches: applied {}/{} patches",
                applied,
                patches.len()
            );
        }

        // Compress and write connection matrix
        let original_matrix_size = (matrix_end - matrix_start) as f64;
        let compressed = compress_connection_matrix(&buf, matrix_start, left_id, right_id);
        eprintln!(
            "  Connection matrix: {:.1} MB → {:.1} MB ({:.1}x ratio, zstd)",
            original_matrix_size / 1024.0 / 1024.0,
            compressed.len() as f64 / 1024.0 / 1024.0,
            original_matrix_size / compressed.len() as f64
        );
        output.extend_from_slice(&compressed);

        lexicon_offset = matrix_end;
    } else {
        lexicon_offset = HEADER_SIZE;
    }

    // ── 3. MARISA trie ──────────────────────────────────────────────
    let (yada_array, after_trie_offset) = parse_yada_array(&buf, lexicon_offset);
    eprintln!(
        "  YADA trie: {} units ({} bytes)",
        yada_array.len(),
        yada_array.len() * 4
    );

    let entries = extract_yada_entries(&yada_array);
    eprintln!("  Extracted {} trie entries", entries.len());

    let marisa_section = build_marisa_section(&entries);
    eprintln!(
        "  MARISA trie: {} bytes ({:.1}x compression)",
        marisa_section.len(),
        (yada_array.len() * 4 + 4) as f64 / marisa_section.len() as f64
    );
    let output_lexicon_offset = output.len();
    output.extend_from_slice(&marisa_section);

    // ── 4. Word ID table + Word params ────────────────────────────────
    let mut post_offset = after_trie_offset;
    let word_id_table_size = read_u32_le(&buf, post_offset) as usize;
    let wit_end = post_offset + 4 + word_id_table_size;
    output.extend_from_slice(&buf[post_offset..wit_end]);
    post_offset = wit_end;

    let output_word_params_offset = output.len();
    let word_params_size = read_u32_le(&buf, post_offset) as usize;
    let wp_data_start = post_offset + 4;
    let wp_end = wp_data_start + word_params_size * 6;

    if let Some(ref adjustments) = cost_adjustments {
        // Write word_params with adjusted costs
        output.extend_from_slice(&buf[post_offset..wp_data_start]); // size u32
        let mut patched = 0u32;
        for wid in 0..word_params_size {
            let entry_off = wp_data_start + wid * 6;
            let left_id = read_i16_le(&buf, entry_off);
            let right_id = read_i16_le(&buf, entry_off + 2);
            let cost = if let Some(&adj_cost) = adjustments.get(&(wid as u32)) {
                patched += 1;
                adj_cost
            } else {
                read_i16_le(&buf, entry_off + 4)
            };
            output.extend_from_slice(&left_id.to_le_bytes());
            output.extend_from_slice(&right_id.to_le_bytes());
            output.extend_from_slice(&cost.to_le_bytes());
        }
        eprintln!(
            "  Word params: patched {}/{} costs from CSV",
            patched, word_params_size
        );
    } else {
        // Copy word_params as-is
        output.extend_from_slice(&buf[post_offset..wp_end]);
    }
    post_offset = wp_end;

    // ── 5. Block-compressed word infos ───────────────────────────────
    let word_infos_start = post_offset;
    let original_wi_size = buf.len() - word_infos_start;

    let compressed_wi = block_compress_word_infos(&buf, word_infos_start, word_params_size);
    eprintln!(
        "  Word infos: {:.1} MB → {:.1} MB ({:.1}x ratio, zstd, {}-record blocks)",
        original_wi_size as f64 / 1024.0 / 1024.0,
        compressed_wi.len() as f64 / 1024.0 / 1024.0,
        original_wi_size as f64 / compressed_wi.len() as f64,
        WI_BLOCK_SIZE,
    );
    output.extend_from_slice(&compressed_wi);

    // ── 6. Swap trie + word_id table to reading-keyed (KKC only) ──────
    //
    // Only when --reading-keyed is set: replace the surface-keyed MARISA trie
    // and word_id table with reading-keyed (hiragana) versions so Viterbi
    // operates on readings. Without this flag, the trie remains surface-keyed
    // for normal tokenization of kanji/kana text.
    if reading_keyed {
        eprintln!("\nSwapping lexicon trie to reading-keyed (hiragana)...");
        let has_synonym_group_ids = version == SYSTEM_DICT_V2 || version == USER_DICT_V3;
        let (reading_marisa, reading_wit) =
            build_reading_keyed_sections(&output, output_lexicon_offset, has_synonym_group_ids);

        // Reassemble: keep header+grammar, swap trie+wit, keep word_params+word_infos
        let word_params_onward = output[output_word_params_offset..].to_vec();
        output.truncate(output_lexicon_offset);
        let output_lexicon_offset = output.len(); // recalculate after truncation
        output.extend_from_slice(&reading_marisa);
        output.extend_from_slice(&reading_wit);
        output.extend_from_slice(&word_params_onward);

        // ── 7. Reading trie (RTRI) for candidate lookup ─────────────────
        eprintln!("\nBuilding reading trie (RTRI) for candidate lookup...");
        let reading_trie_section =
            build_reading_trie_section(&output, output_lexicon_offset, has_synonym_group_ids);
        output.extend_from_slice(&reading_trie_section);
    } else {
        // ── 7. Reading trie (RTRI) for candidate lookup ─────────────────
        eprintln!("\nBuilding reading trie (RTRI) for candidate lookup...");
        let has_synonym_group_ids = version == SYSTEM_DICT_V2 || version == USER_DICT_V3;
        let reading_trie_section =
            build_reading_trie_section(&output, output_lexicon_offset, has_synonym_group_ids);
        output.extend_from_slice(&reading_trie_section);
    }

    // ── Write output ────────────────────────────────────────────────
    fs::write(output_path, &output).unwrap_or_else(|e| {
        eprintln!("Failed to write {}: {}", output_path, e);
        process::exit(1);
    });

    eprintln!(
        "\n  Total: {:.1} MB → {:.1} MB ({:.1}% of original)",
        buf.len() as f64 / 1024.0 / 1024.0,
        output.len() as f64 / 1024.0 / 1024.0,
        output.len() as f64 / buf.len() as f64 * 100.0
    );
    eprintln!("Done: {}", output_path);
}
