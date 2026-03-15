# sudachi-wasm

WebAssembly bindings for the [Sudachi](https://github.com/WorksApplications/sudachi.rs) Japanese
morphological analyzer. Exposes a `Tokenizer` class to JavaScript / TypeScript that accepts a
Sudachi system dictionary and tokenizes Japanese text into morphemes with full metadata.

## Prerequisites

| Tool | Install |
|------|---------|
| Rust + `wasm32-unknown-unknown` target | `rustup target add wasm32-unknown-unknown` |
| [wasm-pack](https://rustwasm.github.io/wasm-pack/) | `cargo install wasm-pack` |
| A Sudachi system dictionary | [sudachi-dictionary](https://github.com/WorksApplications/SudachiDict) release page |

Download a dictionary (e.g. `sudachi-dictionary-*-core.zip`), unzip, and keep the
`system_core.dic` file handy.

## Build

### For browsers (ES module)
```sh
wasm-pack build --target web --release
# output: pkg/
```

### For Node.js (CommonJS)
```sh
wasm-pack build --target nodejs --release
# output: pkg/
```

### For bundlers (webpack / Vite / Rollup)
```sh
wasm-pack build --target bundler --release
# output: pkg/
```

### What makes the `.wasm` small

| Optimisation | Where | Effect |
|---|---|---|
| `opt-level = "z"` | `Cargo.toml` `[profile.release.package.sudachi-wasm]` | Compiles sudachi-wasm code for size |
| `lto = "fat"` | `Cargo.toml` `[profile.release]` | Full cross-crate dead-code elimination |
| `codegen-units = 1` | `Cargo.toml` `[profile.release]` | Single codegen unit for better LTO |
| `panic = "abort"` | `.cargo/config.toml` | Removes ~15-20 KB of unwind tables on WASM |
| `strip = "symbols"` | `Cargo.toml` `[profile.release.package.sudachi-wasm]` | Strips debug symbol names |
| `default-features = false` | `sudachi-wasm/Cargo.toml` | Excludes `csv` + dictionary-build code |
| `wasm-opt -Oz` | wasm-pack (disabled — crashes on this binary; Rust-side LTO achieves equivalent results) | Post-link bytecode optimisation |

The dictionary file (`system_core.dic`, ~71 MB) is **not** bundled in the `.wasm`; it is
fetched/loaded separately at runtime, keeping the binary lean.

## JavaScript API

```ts
class Tokenizer {
  /** Load a tokenizer from the raw bytes of a `.dic` file. */
  constructor(dictBytes: Uint8Array);

  /**
   * Tokenize Japanese text.
   * @param text  Input string.
   * @param mode  "A" (shortest) | "B" (word-level) | "C" (named-entity, default)
   */
  tokenize(text: string, mode?: string): Morpheme[];
}

interface Morpheme {
  surface:        string;    // substring of the input as-is
  dictionaryForm: string;    // base / lemma form
  readingForm:    string;    // katakana reading (フリガナ)
  normalizedForm: string;    // normalized form
  partOfSpeech:   string[];  // up to 6 POS fields, e.g. ["名詞","普通名詞","一般","*","*","*"]
  isOov:          boolean;   // true when the word was not found in the dictionary
  begin:          number;    // start offset (Unicode code-point index, inclusive)
  end:            number;    // end   offset (Unicode code-point index, exclusive)
}
```

## Browser example

> **Why `file://` doesn't work**: browsers block loading ES modules and `.wasm` files over
> `file://` due to CORS policy. You must open the page through an HTTP server.

```sh
# 1. Build for the web
wasm-pack build --target web --release

# 2. Copy the pkg/ folder and system_core.dic next to index.html
cp -r pkg examples/browser/pkg
cp /path/to/system_core.dic examples/browser/

# 3a. Zero-dependency Node.js dev server (no install needed, Node ≥ 18)
node examples/browser/serve.js        # → http://localhost:8080
node examples/browser/serve.js 3000   # custom port

# 3b. Or use Python's built-in server
python3 -m http.server 8080 --directory examples/browser

# 3c. Or any static file server
npx serve examples/browser            # → http://localhost:3000
```

The page lets you type any Japanese sentence, choose a split mode, and see a table of morphemes.

### Expected output for `今日はいい天気ですね。` (mode C)

| # | Surface | Reading | Dictionary form | POS |
|---|---------|---------|-----------------|-----|
| 1 | 今日 | キョウ | 今日 | 名詞 › 普通名詞 › 副詞可能 |
| 2 | は | ハ | は | 助詞 › 係助詞 |
| 3 | いい | イイ | 良い | 形容詞 › 一般 |
| 4 | 天気 | テンキ | 天気 | 名詞 › 普通名詞 › 一般 |
| 5 | です | デス | です | 助動詞 |
| 6 | ね | ネ | ね | 助詞 › 終助詞 |
| 7 | 。 | 。 | 。 | 補助記号 › 句点 |

## Node.js example

```sh
# 1. Build for Node.js
wasm-pack build --target nodejs

# 2. Run (pass the path to your .dic file)
node examples/nodejs/main.mjs /path/to/system_core.dic
```

Expected console output (abbreviated):

```
Input : 今日はいい天気ですね。

── Mode C ────────────────────────────────
┌─────────┬────────┬────────────────┬──────────────────────────┬───────┬────────┐
│ surface │reading │ dictionaryForm │ pos                      │ isOov │ span   │
├─────────┼────────┼────────────────┼──────────────────────────┼───────┼────────┤
│ 今日    │ キョウ │ 今日           │ 名詞-普通名詞-副詞可能   │ false │ [0,2) │
│ は      │ ハ     │ は             │ 助詞-係助詞              │ false │ [2,3) │
│ いい    │ イイ   │ 良い           │ 形容詞-一般              │ false │ [3,5) │
│ 天気    │ テンキ │ 天気           │ 名詞-普通名詞-一般       │ false │ [5,7) │
│ です    │ デス   │ です           │ 助動詞                   │ false │ [7,9) │
│ ね      │ ネ     │ ね             │ 助詞-終助詞              │ false │ [9,10)│
│ 。      │ 。     │ 。             │ 補助記号-句点            │ false │[10,11)│
└─────────┴────────┴────────────────┴──────────────────────────┴───────┴────────┘
```
