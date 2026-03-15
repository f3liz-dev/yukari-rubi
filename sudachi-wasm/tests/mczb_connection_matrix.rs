#![cfg(not(target_arch = "wasm32"))]

use sudachi::dic::connect::ConnectionMatrix;
use zstd::{bulk, dict};

const MAGIC: u32 = 0x4D43_5A42; // "MCZB"
const BLOCK_SIZE: usize = 256;

fn build_mczb_matrix(values: &[i16], num_left: usize, num_right: usize) -> Vec<u8> {
    let num_row_blocks = num_right.div_ceil(BLOCK_SIZE);
    let num_col_blocks = num_left.div_ceil(BLOCK_SIZE);
    let num_blocks = num_row_blocks * num_col_blocks;

    let mut samples = Vec::with_capacity(num_blocks);
    for rb in 0..num_row_blocks {
        for cb in 0..num_col_blocks {
            let mut block = Vec::new();
            for r in (rb * BLOCK_SIZE)..std::cmp::min((rb + 1) * BLOCK_SIZE, num_right) {
                for c in (cb * BLOCK_SIZE)..std::cmp::min((cb + 1) * BLOCK_SIZE, num_left) {
                    let idx = r * num_left + c;
                    block.extend_from_slice(&values[idx].to_le_bytes());
                }
            }
            samples.push(block);
        }
    }

    let dictionary = dict::from_samples(&samples, 1024).expect("train dict");
    let mut compressor = bulk::Compressor::with_dictionary(19, &dictionary).expect("compressor");

    let compressed_blocks: Vec<Vec<u8>> = samples
        .iter()
        .map(|sample| compressor.compress(sample).expect("compress block"))
        .collect();

    let mut buf = Vec::new();
    buf.extend_from_slice(&MAGIC.to_le_bytes());
    buf.extend_from_slice(&(num_left as u16).to_le_bytes());
    buf.extend_from_slice(&(num_right as u16).to_le_bytes());
    buf.extend_from_slice(&(num_blocks as u32).to_le_bytes());
    buf.extend_from_slice(&(dictionary.len() as u32).to_le_bytes());
    buf.extend_from_slice(&dictionary);

    let mut offset = 0u32;
    for block in &compressed_blocks {
        buf.extend_from_slice(&offset.to_le_bytes());
        buf.extend_from_slice(&(block.len() as u32).to_le_bytes());
        offset += block.len() as u32;
    }

    for block in &compressed_blocks {
        buf.extend_from_slice(block);
    }

    buf
}

#[test]
fn loads_plain_zstd_connection_matrix() {
    let num_left = 1024usize;
    let num_right = 1024usize;
    let values: Vec<i16> = (0..num_right)
        .flat_map(|r| {
            (0..num_left).map(move |c| {
                let base = ((r as i32 * 37) - (c as i32 * 19) + ((r ^ c) as i32 * 3)) % 20_000;
                (base - 10_000) as i16
            })
        })
        .collect();

    let buf = build_mczb_matrix(&values, num_left, num_right);
    let (matrix, consumed) = ConnectionMatrix::from_compressed(&buf, 4).expect("parse mczb");

    assert_eq!(matrix.num_left(), num_left);
    assert_eq!(matrix.num_right(), num_right);
    assert_eq!(consumed, buf.len() - 4);

    for right in 0..num_right {
        for left in 0..num_left {
            let expected = values[right * num_left + left];
            assert_eq!(matrix.cost(left as u16, right as u16), expected);
        }
    }
}
