//! Test connection matrix loading and cost lookups
use std::fs;

fn main() {
    let dic_path = std::env::args().nth(1).expect("Usage: test_matrix <path_to.dic>");
    let dic_bytes = fs::read(&dic_path).expect("Failed to read dictionary");
    eprintln!("Loaded {} ({:.1} MB)", dic_path, dic_bytes.len() as f64 / 1024.0 / 1024.0);

    let dict = unsafe { sudachi::dic::DictionaryLoader::read_any_dictionary(&dic_bytes) }
        .expect("Failed to parse dictionary");
    eprintln!("Dictionary parsed!");

    if let Some(ref grammar) = dict.grammar {
        let conn = grammar.conn_matrix();
        eprintln!("Matrix: left={}, right={}", conn.num_left(), conn.num_right());

        // Test some cost lookups
        for (l, r) in &[(0u16, 0u16), (1, 1), (100, 100), (500, 500), (1000, 2000), (5000, 5000)] {
            let c = conn.cost(*l, *r);
            eprintln!("  cost({},{})={}", l, r, c);
        }
        eprintln!("Connection matrix OK!");
    }
}
