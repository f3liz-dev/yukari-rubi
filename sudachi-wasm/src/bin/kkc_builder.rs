//! KKC Dictionary Builder
//!
//! Builds a standalone Kana-Kanji Conversion dictionary from a Sudachi system
//! dictionary. The pipeline has two modes:
//!
//! 1. `--export`: Extract (hiragana, surface, cost, POS) from system dict → CSV
//!    for Julia cost analysis
//! 2. `--build`: Build the KKC dictionary from an adjusted CSV (output of Julia)
//!
//! Usage:
//!   kkc_builder --export <system.dic> <output.csv>
//!   kkc_builder --build  <adjusted.csv> <params.json> <output.kkc>

use rsmarisa::grimoire::io::Writer;
use rsmarisa::{Agent, Keyset};
use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::process;

use sudachi::dic::dictionary::JapaneseDictionary;
use sudachi::dic::subset::InfoSubset;
use sudachi::dic::word_id::WordId;
use sudachi::kana::hiragana_to_katakana;

const KKC_MAGIC: u32 = 0x4B4B_4344; // "KKCD"
const KKC_VERSION: u32 = 1;

// ─── Export Mode ────────────────────────────────────────────────────────────

/// Export all word entries from a Sudachi dictionary to CSV for Julia analysis.
///
/// CSV columns:
///   reading_hiragana, reading_katakana, surface, cost, left_id, right_id,
///   pos_id, pos_str, char_count
fn export_dict(dict_path: &str, output_path: &str) {
    eprintln!("Loading dictionary: {}", dict_path);
    let buf = fs::read(dict_path).unwrap_or_else(|e| {
        eprintln!("Failed to read {}: {}", dict_path, e);
        process::exit(1);
    });

    let dict = JapaneseDictionary::from_system_bytes(buf).unwrap_or_else(|e| {
        eprintln!("Failed to parse dictionary: {}", e);
        process::exit(1);
    });

    let lexicon = dict.lexicon();
    let grammar = dict.grammar();
    let total = lexicon.size();

    let mut out = std::io::BufWriter::new(
        fs::File::create(output_path).unwrap_or_else(|e| {
            eprintln!("Failed to create {}: {}", output_path, e);
            process::exit(1);
        }),
    );

    // Header
    writeln!(
        out,
        "word_id,reading_hiragana,reading_katakana,surface,cost,left_id,right_id,pos_id,pos_str,char_count"
    )
    .unwrap();

    let mut exported = 0u32;
    let mut skipped = 0u32;

    for wid in 0..total {
        let word_id = WordId::new(0, wid);
        let (left_id, right_id, cost) = lexicon.get_word_param(word_id);

        // Skip invalid entries
        if left_id < 0 || right_id < 0 {
            skipped += 1;
            continue;
        }

        let info = match lexicon.get_word_info_subset(
            word_id,
            InfoSubset::SURFACE | InfoSubset::READING_FORM | InfoSubset::POS_ID,
        ) {
            Ok(info) => info,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };

        let surface = info.surface();
        let reading = info.reading_form();

        // Skip empty readings
        if reading.is_empty() || surface.is_empty() {
            skipped += 1;
            continue;
        }

        let reading_katakana = hiragana_to_katakana(reading);

        // Convert katakana to hiragana for the KKC key
        let reading_hiragana = katakana_to_hiragana(&reading_katakana);

        let pos_id = info.pos_id();
        let pos_str = grammar.pos_components(pos_id).join("-");
        let char_count = surface.chars().count();

        // Escape CSV fields (simple: quote fields that contain commas)
        let surface_csv = csv_escape(surface);
        let reading_h_csv = csv_escape(&reading_hiragana);
        let reading_k_csv = csv_escape(&reading_katakana);
        let pos_csv = csv_escape(&pos_str);

        writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{}",
            wid,
            reading_h_csv,
            reading_k_csv,
            surface_csv,
            cost,
            left_id,
            right_id,
            pos_id,
            pos_csv,
            char_count,
        )
        .unwrap();

        exported += 1;
    }

    eprintln!(
        "Exported {} entries ({} skipped) to {}",
        exported, skipped, output_path
    );
}

fn csv_escape(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Convert katakana to hiragana (U+30A1..U+30F6 → U+3041..U+3096).
fn katakana_to_hiragana(s: &str) -> String {
    s.chars()
        .map(|c| {
            if ('\u{30A1}'..='\u{30F6}').contains(&c) {
                char::from_u32(c as u32 - 0x60).unwrap_or(c)
            } else {
                c
            }
        })
        .collect()
}

// ─── Build Mode ─────────────────────────────────────────────────────────────

/// A single KKC entry parsed from the adjusted CSV.
struct KkcEntry {
    reading_hiragana: String,
    surface: String,
    cost: i16,
    left_id: i16,
    right_id: i16,
    pos_id: u16,
}

/// Parameters from Julia optimization (params.json).
#[derive(Default)]
struct KkcParams {
    alpha: i16,
    beta: i16,
}

fn parse_params(path: &str) -> KkcParams {
    let content = fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Failed to read params file {}: {}", path, e);
        process::exit(1);
    });

    // Simple JSON parsing (avoid heavy dependency)
    let alpha = extract_json_i16(&content, "alpha").unwrap_or(500);
    let beta = extract_json_i16(&content, "beta").unwrap_or(12000);

    eprintln!("  Parameters: α={}, β={}", alpha, beta);
    KkcParams { alpha, beta }
}

fn extract_json_i16(json: &str, key: &str) -> Option<i16> {
    let pattern = format!("\"{}\"", key);
    let pos = json.find(&pattern)?;
    let after_key = &json[pos + pattern.len()..];
    let colon_pos = after_key.find(':')?;
    let value_str = after_key[colon_pos + 1..].trim();
    // Find end of number
    let end = value_str
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(value_str.len());
    value_str[..end].trim().parse::<i16>().ok()
}

fn load_adjusted_csv(path: &str) -> Vec<KkcEntry> {
    let file = fs::File::open(path).unwrap_or_else(|e| {
        eprintln!("Failed to open {}: {}", path, e);
        process::exit(1);
    });
    let reader = BufReader::new(file);
    let mut entries = Vec::new();

    for (i, line) in reader.lines().enumerate() {
        let line = line.unwrap();
        if i == 0 {
            continue; // skip header
        }

        let fields = parse_csv_line(&line);
        // CSV format: word_id,reading_h,reading_k,surface,cost,left_id,right_id,pos_id,...
        if fields.len() < 8 {
            continue;
        }

        // fields[0] = word_id (skip for KKC build, used by dic_converter)
        let reading_hiragana = fields[1].clone();
        // fields[2] = reading_katakana (skip)
        let surface = fields[3].clone();
        let cost: i16 = match fields[4].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let left_id: i16 = match fields[5].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let right_id: i16 = match fields[6].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        let pos_id: u16 = match fields[7].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };

        entries.push(KkcEntry {
            reading_hiragana,
            surface,
            cost,
            left_id,
            right_id,
            pos_id,
        });
    }

    eprintln!("  Loaded {} entries from {}", entries.len(), path);
    entries
}

/// Minimal CSV line parser handling quoted fields.
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    current.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                current.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == ',' {
            fields.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
    }
    fields.push(current);
    fields
}

/// Build KKC dictionary from adjusted CSV and Julia parameters.
///
/// Dictionary format (KKCD):
/// ```text
/// [MAGIC: u32]           = 0x4B4B4344
/// [VERSION: u32]         = 1
/// [num_entries: u32]     = total unique entries
/// [α: i16]               = compound boost
/// [β: i16]               = identity penalty
/// [trie_size: u32]
/// [trie_data: bytes]     = MARISA trie (keys = hiragana reading bytes)
/// [num_keys: u32]
/// [id_to_offset: u32 × num_keys]
/// [entry_table_size: u32]
/// [entry_table: bytes]   = per key: [count: u16][entries]
///   each entry:           [surface_off: u32][surface_len: u16][cost: i16]
///                         [left_id: i16][right_id: i16][pos_id: u16]
/// [string_pool_size: u32]
/// [string_pool: bytes]   = concatenated surface strings
/// ```
fn build_kkc_dict(csv_path: &str, params_path: &str, output_path: &str) {
    eprintln!("Building KKC dictionary...");
    let params = parse_params(params_path);
    let entries = load_adjusted_csv(csv_path);

    // Group entries by hiragana reading
    let mut reading_map: BTreeMap<String, Vec<&KkcEntry>> = BTreeMap::new();
    for entry in &entries {
        reading_map
            .entry(entry.reading_hiragana.clone())
            .or_default()
            .push(entry);
    }

    eprintln!("  {} unique readings", reading_map.len());

    // Build string pool (deduplicated surface strings)
    let mut string_pool = Vec::new();
    let mut string_offsets: BTreeMap<String, (u32, u16)> = BTreeMap::new();

    for entry in &entries {
        if !string_offsets.contains_key(&entry.surface) {
            let offset = string_pool.len() as u32;
            let len = entry.surface.len() as u16;
            string_pool.extend_from_slice(entry.surface.as_bytes());
            string_offsets.insert(entry.surface.clone(), (offset, len));
        }
    }

    // Build entry table: for each reading, serialize its entries
    let mut entry_table = Vec::new();
    let mut trie_entries: Vec<(Vec<u8>, u32)> = Vec::new();

    for (reading, word_entries) in &reading_map {
        let offset = entry_table.len() as u32;
        let count = word_entries.len().min(u16::MAX as usize) as u16;

        entry_table.extend_from_slice(&count.to_le_bytes());
        for e in word_entries.iter().take(u16::MAX as usize) {
            let (surf_off, surf_len) = string_offsets[&e.surface];
            entry_table.extend_from_slice(&surf_off.to_le_bytes());
            entry_table.extend_from_slice(&surf_len.to_le_bytes());
            entry_table.extend_from_slice(&e.cost.to_le_bytes());
            entry_table.extend_from_slice(&e.left_id.to_le_bytes());
            entry_table.extend_from_slice(&e.right_id.to_le_bytes());
            entry_table.extend_from_slice(&e.pos_id.to_le_bytes());
        }

        trie_entries.push((reading.as_bytes().to_vec(), offset));
    }

    // Build MARISA trie
    let mut keyset = Keyset::new();
    for (key, _) in &trie_entries {
        keyset
            .push_back_bytes(key, 1.0)
            .expect("failed to add reading to keyset");
    }
    let mut trie = rsmarisa::Trie::new();
    trie.build(&mut keyset, 0);

    let num_keys = trie.num_keys();
    let mut id_to_offset = vec![0u32; num_keys];
    let mut agent = Agent::new();
    for (key, offset_val) in &trie_entries {
        agent.set_query_bytes(key);
        assert!(trie.lookup(&mut agent), "reading not found after build");
        id_to_offset[agent.key().id()] = *offset_val;
    }

    let mut writer = Writer::from_vec(Vec::new());
    trie.write(&mut writer).expect("failed to serialize trie");
    let trie_bytes = writer.into_inner().expect("failed to get trie bytes");

    // Assemble output
    let mut output = Vec::new();

    // Header
    output.extend_from_slice(&KKC_MAGIC.to_le_bytes());
    output.extend_from_slice(&KKC_VERSION.to_le_bytes());
    output.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    output.extend_from_slice(&params.alpha.to_le_bytes());
    output.extend_from_slice(&params.beta.to_le_bytes());

    // Trie section
    output.extend_from_slice(&(trie_bytes.len() as u32).to_le_bytes());
    output.extend_from_slice(&trie_bytes);
    output.extend_from_slice(&(num_keys as u32).to_le_bytes());
    for &off in &id_to_offset {
        output.extend_from_slice(&off.to_le_bytes());
    }

    // Entry table
    output.extend_from_slice(&(entry_table.len() as u32).to_le_bytes());
    output.extend_from_slice(&entry_table);

    // String pool
    output.extend_from_slice(&(string_pool.len() as u32).to_le_bytes());
    output.extend_from_slice(&string_pool);

    fs::write(output_path, &output).unwrap_or_else(|e| {
        eprintln!("Failed to write {}: {}", output_path, e);
        process::exit(1);
    });

    eprintln!(
        "\nKKC dictionary built: {} ({:.1} KB)",
        output_path,
        output.len() as f64 / 1024.0
    );
    eprintln!("  {} entries, {} unique readings", entries.len(), num_keys);
    eprintln!(
        "  Trie: {:.1} KB, Entries: {:.1} KB, Strings: {:.1} KB",
        trie_bytes.len() as f64 / 1024.0,
        entry_table.len() as f64 / 1024.0,
        string_pool.len() as f64 / 1024.0,
    );
}

// ─── KKC Adjust Mode ────────────────────────────────────────────────────────
//
// POS-based cost adjustment in Rust (replaces Julia kkc_costs.jl when unavailable).
// Follows the mecab-as-kkc philosophy: adjust word costs so common content words
// are preferred, functional words are penalized, and reading ambiguity is considered.

/// POS-based cost delta: negative = prefer, positive = penalize.
fn pos_cost_delta(pos_str: &str) -> i32 {
    let major = pos_str.split('-').next().unwrap_or("");
    match major {
        "名詞" => -200,   // nouns — strongly prefer for KKC
        "動詞" => -150,   // verbs — prefer
        "形容詞" => -100, // adjectives — prefer
        "形状詞" => -80,  // na-adjectives — prefer
        "副詞" => -50,    // adverbs — slight prefer
        "連体詞" => -30,  // adnominals
        "代名詞" => -50,  // pronouns
        "助詞" => 200,    // particles — penalize (usually kept as hiragana)
        "助動詞" => 200,  // auxiliary verbs — penalize
        "記号" => 300,    // symbols — strongly penalize
        "補助記号" => 300, // supplementary symbols
        "感動詞" => 100,  // interjections — mild penalize
        "接続詞" => 100,  // conjunctions — mild penalize
        _ => 0,
    }
}

/// Check if a surface contains only katakana (ァ-ヶー).
fn is_all_katakana(s: &str) -> bool {
    s.chars().all(|c| {
        ('\u{30A1}'..='\u{30F6}').contains(&c)
            || c == '\u{30FC}' // ー
            || c == '\u{30F3}' // ン (already in range but explicit)
    })
}

/// Check if a surface contains only hiragana (ぁ-ん).
fn is_all_hiragana(s: &str) -> bool {
    s.chars()
        .all(|c| ('\u{3041}'..='\u{3096}').contains(&c) || c == '\u{30FC}')
}

/// Check if surface contains Latin/ASCII characters.
fn contains_latin(s: &str) -> bool {
    s.chars().any(|c| c.is_ascii_alphabetic())
}

/// Apply POS-aware cost adjustments to exported CSV, producing an adjusted CSV.
///
/// This implements a subset of what kkc_costs.jl does:
/// 1. POS-based cost delta (nouns preferred, particles penalized)
/// 2. Script-mismatch penalty (hiragana reading → Latin surface)
/// 3. Frequency rank within reading group (common entries get bonus)
/// 4. Single-hiragana penalty (prevents は→者 type conversions)
/// 5. Reading ambiguity bonus (unambiguous readings get boost)
///
/// Also generates params.json with α (compound boost) and β (identity penalty).
fn kkc_adjust(input_csv: &str, output_dir: &str) {
    eprintln!("Running Rust-based KKC cost adjustment...");

    let file = fs::File::open(input_csv).unwrap_or_else(|e| {
        eprintln!("Failed to open {}: {}", input_csv, e);
        process::exit(1);
    });
    let reader = BufReader::new(file);

    // Parse all entries
    struct Entry {
        word_id: String,
        reading_h: String,
        reading_k: String,
        surface: String,
        cost: i32,
        left_id: String,
        right_id: String,
        pos_id: String,
        pos_str: String,
        char_count: String,
    }

    let mut entries: Vec<Entry> = Vec::new();
    for (i, line) in reader.lines().enumerate() {
        let line = line.unwrap();
        if i == 0 {
            continue;
        }
        let fields = parse_csv_line(&line);
        if fields.len() < 10 {
            continue;
        }
        let cost: i32 = match fields[4].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        entries.push(Entry {
            word_id: fields[0].clone(),
            reading_h: fields[1].clone(),
            reading_k: fields[2].clone(),
            surface: fields[3].clone(),
            cost,
            left_id: fields[5].clone(),
            right_id: fields[6].clone(),
            pos_id: fields[7].clone(),
            pos_str: fields[8].clone(),
            char_count: fields[9].clone(),
        });
    }

    eprintln!("  Loaded {} entries", entries.len());

    // Group entries by reading for frequency ranking
    let mut reading_groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, e) in entries.iter().enumerate() {
        reading_groups
            .entry(e.reading_h.clone())
            .or_default()
            .push(i);
    }

    // Apply cost adjustments
    let mut adjusted_costs: Vec<i32> = entries.iter().map(|e| e.cost).collect();
    let mut stats = [0i64; 4]; // [pos_adjusted, script_penalized, freq_adjusted, hiragana_penalized]

    for (reading, indices) in &reading_groups {
        let group_size = indices.len();

        // Sort within group by original cost for frequency ranking
        let mut sorted_indices: Vec<usize> = indices.clone();
        sorted_indices.sort_by_key(|&i| entries[i].cost);

        for (rank, &idx) in sorted_indices.iter().enumerate() {
            let e = &entries[idx];
            let mut delta: i32 = 0;

            // 1. POS-based delta
            let pos_delta = pos_cost_delta(&e.pos_str);
            if pos_delta != 0 {
                delta += pos_delta;
                stats[0] += 1;
            }

            // 2. Script-mismatch penalty: hiragana reading → Latin/katakana surface
            if contains_latin(&e.surface) {
                delta += 6000;
                stats[1] += 1;
            } else if is_all_katakana(&e.surface) && !is_all_katakana(&e.reading_k) {
                // Katakana surface for a non-katakana word (e.g., デス for です)
                delta += 1000;
                stats[1] += 1;
            }

            // 3. Frequency rank within reading group
            if group_size > 1 {
                if rank == 0 {
                    delta -= 300; // top entry gets bonus
                } else if rank == group_size - 1 {
                    delta += 200; // worst entry gets penalty
                } else {
                    // Linear interpolation between bonus and penalty
                    let frac = rank as f64 / (group_size - 1) as f64;
                    delta += (-300.0 + 500.0 * frac) as i32;
                }
                stats[2] += 1;
            }

            // 4. Reading ambiguity bonus: unambiguous readings get boost
            if group_size <= 3 {
                delta -= 100;
            }

            // 5. Single-character reading penalty: prevent は→者, ね→眠 etc.
            //    Penalize entries where a single-char hiragana reading maps to kanji.
            if reading.chars().count() == 1 && !is_all_hiragana(&e.surface) {
                delta += 800;
                stats[3] += 1;
            }

            // 6. Surface/reading length ratio penalty
            let surf_len = e.surface.chars().count() as f64;
            let read_len = reading.chars().count().max(1) as f64;
            if surf_len / read_len > 2.5 {
                delta += 2000;
            }

            let new_cost = (e.cost + delta).max(100);
            adjusted_costs[idx] = new_cost;
        }
    }

    eprintln!(
        "  Adjustments: POS={}, script={}, freq={}, hiragana={}",
        stats[0], stats[1], stats[2], stats[3]
    );

    // Compute α and β parameters
    // α: compound boost — estimated from median single-char vs multi-char cost ratio
    let single_char_costs: Vec<i32> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.surface.chars().count() == 1)
        .map(|(i, _)| adjusted_costs[i])
        .collect();
    let multi_char_costs: Vec<i32> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.surface.chars().count() >= 2)
        .map(|(i, _)| adjusted_costs[i])
        .collect();

    let alpha = if !single_char_costs.is_empty() && !multi_char_costs.is_empty() {
        let mut sc = single_char_costs.clone();
        let mut mc = multi_char_costs.clone();
        sc.sort();
        mc.sort();
        let median_s = sc[sc.len() / 2];
        let median_m = mc[mc.len() / 2];
        (median_s - median_m).max(100).min(2000) as i16
    } else {
        500i16
    };

    // β: identity penalty — 95th percentile of single-char costs
    let beta = if !single_char_costs.is_empty() {
        let mut sc = single_char_costs;
        sc.sort();
        let p95_idx = (sc.len() as f64 * 0.95) as usize;
        sc[p95_idx.min(sc.len() - 1)].min(i16::MAX as i32) as i16
    } else {
        12000i16
    };

    // Write adjusted CSV
    let adjusted_path = format!("{}/adjusted.csv", output_dir);
    let mut out = std::io::BufWriter::new(
        fs::File::create(&adjusted_path).unwrap_or_else(|e| {
            eprintln!("Failed to create {}: {}", adjusted_path, e);
            process::exit(1);
        }),
    );

    writeln!(
        out,
        "word_id,reading_hiragana,reading_katakana,surface,cost,left_id,right_id,pos_id,pos_str,char_count"
    )
    .unwrap();

    for (i, e) in entries.iter().enumerate() {
        writeln!(
            out,
            "{},{},{},{},{},{},{},{},{},{}",
            e.word_id,
            csv_escape(&e.reading_h),
            csv_escape(&e.reading_k),
            csv_escape(&e.surface),
            adjusted_costs[i],
            e.left_id,
            e.right_id,
            e.pos_id,
            csv_escape(&e.pos_str),
            e.char_count,
        )
        .unwrap();
    }
    drop(out);
    eprintln!("  Wrote adjusted costs to {}", adjusted_path);

    // Write params.json
    let params_path = format!("{}/params.json", output_dir);
    let params_content = format!(
        "{{\n  \"alpha\": {},\n  \"beta\": {}\n}}\n",
        alpha, beta
    );
    fs::write(&params_path, params_content).unwrap_or_else(|e| {
        eprintln!("Failed to write {}: {}", params_path, e);
        process::exit(1);
    });
    eprintln!("  Parameters: α={}, β={}", alpha, beta);
    eprintln!("  Wrote parameters to {}", params_path);
}

// ─── Main ───────────────────────────────────────────────────────────────────

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        print_usage(&args[0]);
        process::exit(1);
    }

    match args[1].as_str() {
        "--export" => {
            if args.len() != 4 {
                eprintln!("Usage: {} --export <system.dic> <output.csv>", args[0]);
                process::exit(1);
            }
            export_dict(&args[2], &args[3]);
        }
        "--build" => {
            if args.len() != 5 {
                eprintln!(
                    "Usage: {} --build <adjusted.csv> <params.json> <output.kkc>",
                    args[0]
                );
                process::exit(1);
            }
            build_kkc_dict(&args[2], &args[3], &args[4]);
        }
        "--kkc-adjust" => {
            if args.len() != 4 {
                eprintln!(
                    "Usage: {} --kkc-adjust <words_export.csv> <output_dir>",
                    args[0]
                );
                process::exit(1);
            }
            kkc_adjust(&args[2], &args[3]);
        }
        _ => {
            print_usage(&args[0]);
            process::exit(1);
        }
    }
}

fn print_usage(prog: &str) {
    eprintln!("KKC Dictionary Builder");
    eprintln!();
    eprintln!("Usage:");
    eprintln!("  {} --export <system.dic> <output.csv>", prog);
    eprintln!("    Export word entries from Sudachi dictionary to CSV for Julia analysis");
    eprintln!();
    eprintln!(
        "  {} --build <adjusted.csv> <params.json> <output.kkc>",
        prog
    );
    eprintln!("    Build KKC dictionary from Julia-adjusted CSV and parameters");
    eprintln!();
    eprintln!(
        "  {} --kkc-adjust <words_export.csv> <output_dir>",
        prog
    );
    eprintln!("    Apply POS-based cost adjustment in Rust (Julia-free alternative)");
    eprintln!("    Outputs: adjusted.csv + params.json in output_dir");
}
