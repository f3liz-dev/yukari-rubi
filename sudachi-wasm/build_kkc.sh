#!/bin/bash
#
# build_kkc.sh — Build KKC-optimized Sudachi dictionaries
#
# Pipeline:
#   1. Compile dic_converter + kkc_builder (Rust)
#   2. Export word entries from source .dic → CSV (with word_id)
#   3. Run Julia cost optimizer → adjusted CSV + params.json
#   4. Convert YADA → MARISA with adjusted costs baked into word_params
#
# The output is a standard Sudachi .dic that plain sudachi can load.
# KKC cost adjustments are applied to word_params during conversion.
#
# Usage:
#   ./build_kkc.sh <variant> [source_dic_path] [output_dir]
#
#   variant:  small | core | full | all
#   source_dic_path:  path to original system_{variant}.dic (YADA format)
#                     defaults to sudachi.rs/resources/system.dic
#   output_dir:       defaults to examples/browser/
#
# Examples:
#   ./build_kkc.sh core                           # build system_core.dic
#   ./build_kkc.sh core path/to/system_core.dic
#   ./build_kkc.sh full system_full.dic ./output/
#   ./build_kkc.sh all                            # build all variants
#
# Requirements:
#   - Rust toolchain (cargo)
#   - Julia (julia) — optional, falls back to original costs
#   - Sudachi system dictionary in YADA format (original download)
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
JULIA_DIR="$SCRIPT_DIR/julia"
BROWSER_DIR="$SCRIPT_DIR/examples/browser"
RESOURCES_DIR="$SCRIPT_DIR/sudachi.rs/resources"

# ── Arguments ────────────────────────────────────────────────────────────────

if [ $# -lt 1 ]; then
    echo "Usage: $0 <variant> [source_dic_path] [output_dir]" >&2
    echo "" >&2
    echo "Variants: small, core, full, all" >&2
    echo "  all: builds all available variants" >&2
    echo "" >&2
    echo "The output is a standard Sudachi .dic with KKC-optimized costs" >&2
    echo "and a reading trie. Plain sudachi can load it normally." >&2
    exit 1
fi

VARIANT="$1"

# Handle 'all' variant by recursing
if [ "$VARIANT" = "all" ]; then
    for v in small core full; do
        DIC="${RESOURCES_DIR}/system_${v}.dic"
        if [ ! -f "$DIC" ]; then
            DIC="${BROWSER_DIR}/system_${v}.dic"
        fi
        if [ -f "$DIC" ]; then
            echo ""
            echo "════════════════════════════════════════════════════════"
            echo "  Building KKC dictionary: $v"
            echo "════════════════════════════════════════════════════════"
            "$0" "$v" "$DIC" "${2:-$BROWSER_DIR}"
        else
            echo "Skipping $v: no source dictionary found"
        fi
    done
    exit 0
fi

# Validate variant
case "$VARIANT" in
    small|core|full) ;;
    *) echo "Error: unknown variant '$VARIANT'. Use: small, core, full, or all" >&2; exit 1 ;;
esac

SOURCE_DIC="${2:-$RESOURCES_DIR/system.dic}"
OUTPUT_DIR="${3:-$BROWSER_DIR}"

if [ ! -f "$SOURCE_DIC" ]; then
    echo "Error: $SOURCE_DIC not found" >&2
    echo "Provide the path to a YADA-format system_${VARIANT}.dic" >&2
    exit 1
fi

mkdir -p "$OUTPUT_DIR"

# Temporary working directory for intermediate files
WORK_DIR=$(mktemp -d)
trap "rm -rf '$WORK_DIR'" EXIT

echo "════════════════════════════════════════════════════════"
echo "  KKC Dictionary Build: $VARIANT"
echo "════════════════════════════════════════════════════════"
echo "  Input:  $SOURCE_DIC"
echo "  Output: $OUTPUT_DIR/system_${VARIANT}.dic"
echo ""

# ── Step 1: Build tools ──────────────────────────────────────────────────────

echo "─── Step 1: Compiling tools ───"
cd "$SCRIPT_DIR"
cargo build --release --bin dic_converter --bin kkc_builder 2>&1 | tail -3
DIC_CONVERTER="$SCRIPT_DIR/target/release/dic_converter"
KKC_BUILDER="$SCRIPT_DIR/target/release/kkc_builder"
echo "  Built: dic_converter, kkc_builder"
echo ""

# ── Step 2: Export dictionary to CSV ─────────────────────────────────────────

EXPORT_CSV="$WORK_DIR/words_export.csv"
echo "─── Step 2: Exporting dictionary entries ───"
"$KKC_BUILDER" --export "$SOURCE_DIC" "$EXPORT_CSV"
WORD_COUNT=$(wc -l < "$EXPORT_CSV")
echo "  Exported $((WORD_COUNT - 1)) entries"
echo ""

# ── Step 3: Julia cost optimization ──────────────────────────────────────────

JULIA_SCRIPT="$JULIA_DIR/kkc_costs.jl"
MATRIX_PATCH_SCRIPT="$JULIA_DIR/matrix_patches.jl"
echo "─── Step 3: Running cost optimizer ───"
if command -v julia &>/dev/null && [ -f "$JULIA_SCRIPT" ]; then
    echo "  Using Julia cost optimizer"
    # Use jlmarisa project for dependencies (Statistics, Printf, Dates)
    JLMARISA_PROJECT="$SCRIPT_DIR/../jlmarisa"
    if [ -d "$JLMARISA_PROJECT" ]; then
        JULIA_CMD="julia --project=$JLMARISA_PROJECT"
    else
        JULIA_CMD="julia"
    fi

    # Step 3a: Cost optimization
    $JULIA_CMD "$JULIA_SCRIPT" "$EXPORT_CSV" "$WORK_DIR"
    COST_CSV_FLAG="--cost-csv $WORK_DIR/adjusted.csv"

    # Step 3b: Matrix patches
    if [ -f "$MATRIX_PATCH_SCRIPT" ]; then
        echo ""
        echo "─── Step 3b: Generating matrix patches ───"
        $JULIA_CMD "$MATRIX_PATCH_SCRIPT" "$EXPORT_CSV" "$WORK_DIR/matrix_patches.csv"
        if [ -f "$WORK_DIR/matrix_patches.csv" ]; then
            MATRIX_PATCH_FLAG="--matrix-patches $WORK_DIR/matrix_patches.csv"
        else
            MATRIX_PATCH_FLAG=""
        fi
    else
        MATRIX_PATCH_FLAG=""
    fi
else
    echo "  Julia not found. Using Rust-based KKC cost adjustment."
    "$KKC_BUILDER" --kkc-adjust "$EXPORT_CSV" "$WORK_DIR"
    COST_CSV_FLAG="--cost-csv $WORK_DIR/adjusted.csv"
    MATRIX_PATCH_FLAG=""
fi
echo ""

# ── Step 4: Convert YADA → MARISA with adjusted costs ───────────────────────

OUTPUT_DIC="$OUTPUT_DIR/system_${VARIANT}.dic"
echo "─── Step 4: Converting dictionary (YADA → MARISA + RTRI) ───"
# shellcheck disable=SC2086
"$DIC_CONVERTER" "$SOURCE_DIC" "$OUTPUT_DIC" $COST_CSV_FLAG $MATRIX_PATCH_FLAG
echo ""

# ── Summary ──────────────────────────────────────────────────────────────────

echo "════════════════════════════════════════════════════════"
echo "  Build Complete: system_${VARIANT}.dic"
echo "════════════════════════════════════════════════════════"
echo "  Dictionary: $OUTPUT_DIC ($(du -h "$OUTPUT_DIC" | cut -f1))"
echo "  Format: MARISA trie + reading trie + KKC costs"
echo "  Compatible with plain sudachi"
if [ -f "$WORK_DIR/params.json" ]; then
    echo "  α = $(grep -o '"alpha": [0-9-]*' "$WORK_DIR/params.json" | grep -o '[0-9-]*')"
    echo "  β = $(grep -o '"beta": [0-9-]*' "$WORK_DIR/params.json" | grep -o '[0-9-]*')"
fi
echo "════════════════════════════════════════════════════════"
