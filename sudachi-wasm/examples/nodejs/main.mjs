/**
 * Node.js example – tokenize Japanese text with Sudachi WASM.
 *
 * Build first:
 *   wasm-pack build --target nodejs ../../  (from this directory)
 *   # or:  wasm-pack build --target nodejs   (from sudachi-wasm/)
 *
 * Then run:
 *   node main.mjs  [path/to/system_core.dic]
 */

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, resolve } from "node:path";

// wasm-pack --target nodejs emits CommonJS; use createRequire to load it from ESM.
const require = createRequire(import.meta.url);
const __dir = dirname(fileURLToPath(import.meta.url));

// Path to the wasm-pack output (adjust if you placed pkg elsewhere)
const { Tokenizer } = require(resolve(__dir, "../../pkg/sudachi_wasm.js"));

// ── Load dictionary ──────────────────────────────────────────────────────────
const dicPath = process.argv[2] ?? resolve(__dir, "system_core.dic");
console.log(`Loading dictionary: ${dicPath}`);
const dictBytes = new Uint8Array(readFileSync(dicPath).buffer);

// ── Build tokenizer ──────────────────────────────────────────────────────────
const tokenizer = new Tokenizer(dictBytes);
console.log("Tokenizer ready.\n");

// ── Tokenize example sentence ────────────────────────────────────────────────
const sentence = "今日はいい天気ですね。";
console.log(`Input : ${sentence}\n`);

for (const mode of ["C", "B", "A"]) {
  const morphemes = tokenizer.tokenize(sentence, mode);
  console.log(`── Mode ${mode} ────────────────────────────────`);
  console.table(
    morphemes.map((m) => ({
      surface:        m.surface,
      reading:        m.readingForm,
      dictionaryForm: m.dictionaryForm,
      pos:            m.partOfSpeech.slice(0, 2).join("-"),
      "pos (full)":   m.partOfSpeech.join("-"),
      isOov:          m.isOov,
      span:           `[${m.begin},${m.end})`,
    }))
  );
  console.log();
}
