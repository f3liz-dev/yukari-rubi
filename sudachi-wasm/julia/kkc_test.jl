"""
    KKC Viterbi Test Harness

Fast iteration loop for tuning KKC costs without rebuilding the MARISA dictionary.

Parses the connection matrix from the original binary dictionary,
loads word costs from the (adjusted) CSV, and runs a Viterbi lattice search
to simulate Sudachi's KKC conversion.

Usage:
    julia kkc_test.jl <system.dic> <export_or_adjusted.csv> [matrix_patches.csv]

The CSV can be either:
  - Raw export from `kkc_builder --export` (original costs)
  - Adjusted CSV from `kkc_costs.jl` (tuned costs)

Typical tuning loop:
    export CSV=words_export.csv
    export DIC=sudachi.rs/resources/system.dic
    # 1. Export once:
    kkc_builder --export \$DIC /tmp/kkc_tune/\$CSV
    # 2. Tune loop:
    julia julia/kkc_costs.jl /tmp/kkc_tune/\$CSV /tmp/kkc_tune
    julia julia/matrix_patches.jl /tmp/kkc_tune/\$CSV /tmp/kkc_tune/matrix_patches.csv
    julia julia/kkc_test.jl \$DIC /tmp/kkc_tune/adjusted.csv /tmp/kkc_tune/matrix_patches.csv
    # 3. Edit julia/kkc_costs.jl, repeat from step 2
"""

using Printf

# ─── Constants ───────────────────────────────────────────────────────────────

const HEADER_SIZE = 272
const OOV_COST = Int32(20000)
const BOS_EOS_ID = Int16(0)

# ─── Script Detection ───────────────────────────────────────────────────────

is_hiragana_char(c::Char) = 'ぁ' ≤ c ≤ 'ゖ'
is_katakana_char(c::Char) = 'ァ' ≤ c ≤ 'ヶ'
is_kana_char(c::Char) = is_hiragana_char(c) || is_katakana_char(c)
is_latin_char(c::Char) = 'A' ≤ c ≤ 'Z' || 'a' ≤ c ≤ 'z' || 'Ａ' ≤ c ≤ 'Ｚ' || 'ａ' ≤ c ≤ 'ｚ'
is_all_hiragana(s::AbstractString) = !isempty(s) && all(is_hiragana_char, s)
is_all_latin(s::AbstractString) = !isempty(s) && all(is_latin_char, s)
is_all_kana(s::AbstractString) = !isempty(s) && all(is_kana_char, s)

is_content_pos(pos::AbstractString) = any(startswith(pos, p) for p in ["名詞","動詞","形容詞","副詞","連体詞","形状詞","代名詞"])
is_functional_pos(pos::AbstractString) = any(startswith(pos, p) for p in ["助詞","助動詞","補助記号","記号","空白","接続詞","感動詞"])

# ─── CSV Parsing ─────────────────────────────────────────────────────────────

function parse_csv_fields(line::AbstractString)
    fields = String[]
    current = IOBuffer()
    in_quotes = false
    i = 1
    chars = collect(line)
    n = length(chars)
    while i ≤ n
        c = chars[i]
        if in_quotes
            if c == '"'
                if i < n && chars[i+1] == '"'
                    write(current, '"')
                    i += 1
                else
                    in_quotes = false
                end
            else
                write(current, c)
            end
        elseif c == '"'
            in_quotes = true
        elseif c == ','
            push!(fields, String(take!(current)))
        else
            write(current, c)
        end
        i += 1
    end
    push!(fields, String(take!(current)))
    fields
end

# ─── Connection Matrix ──────────────────────────────────────────────────────

struct ConnMatrix
    data::Vector{Int16}
    num_left::Int
    num_right::Int
end

"""
    load_connection_matrix(dic_path) → ConnMatrix

Parse the connection matrix from a Sudachi binary dictionary (YADA format).
The grammar section starts at byte offset HEADER_SIZE (272).
"""
function load_connection_matrix(dic_path::String)
    buf = read(dic_path)

    # Grammar section starts at HEADER_SIZE
    off = HEADER_SIZE + 1  # Julia is 1-indexed

    # Read POS count
    pos_count = reinterpret(UInt16, buf[off:off+1])[1]
    off += 2

    # Skip POS string table (each POS = 6 UTF-16LE strings with variable-length prefix)
    for _ in 1:pos_count
        for _ in 1:6
            str_len = buf[off]
            if str_len >= 128
                actual = ((Int(str_len) & 0x7f) << 8) | Int(buf[off+1])
                off += 2 + actual * 2
            else
                off += 1 + Int(str_len) * 2
            end
        end
    end

    # Read left/right ID counts
    num_left = reinterpret(UInt16, buf[off:off+1])[1] |> Int
    off += 2
    num_right = reinterpret(UInt16, buf[off:off+1])[1] |> Int
    off += 2

    # Read matrix data: num_left × num_right i16 values
    matrix_size = num_left * num_right
    matrix_end = off + matrix_size * 2 - 1
    if matrix_end > length(buf)
        error("Matrix extends beyond file: need $(matrix_end) bytes, file is $(length(buf))")
    end

    # Copy to properly-aligned array
    data = Vector{Int16}(undef, matrix_size)
    for i in 1:matrix_size
        byte_off = off + (i-1)*2
        data[i] = reinterpret(Int16, buf[byte_off:byte_off+1])[1]
    end

    @printf(stderr, "  Connection matrix: %d × %d (%d entries, %.1f MB)\n",
        num_left, num_right, matrix_size, matrix_size * 2 / 1024 / 1024)
    ConnMatrix(data, num_left, num_right)
end

"""
    conn_cost(m, prev_right_id, cur_left_id) → Int32

Look up the connection cost between two adjacent nodes.
Follows Sudachi convention: cost(left=prev.right_id, right=cur.left_id).
Index = right * num_left + left (0-based), then +1 for Julia.
"""
function conn_cost(m::ConnMatrix, prev_right_id::Int16, cur_left_id::Int16)
    idx = Int(cur_left_id) * m.num_left + Int(prev_right_id) + 1
    Int32(m.data[idx])
end

"""
    apply_matrix_patches!(m, patches_path)

Load and apply matrix patches CSV to the connection matrix.
"""
function apply_matrix_patches!(m::ConnMatrix, patches_path::String)
    count = 0
    open(patches_path, "r") do io
        readline(io)  # skip header
        for line in eachline(io)
            fields = split(line, ',')
            length(fields) >= 3 || continue
            left_id = tryparse(Int, fields[1])
            right_id = tryparse(Int, fields[2])
            delta = tryparse(Int16, fields[3])
            isnothing(left_id) && continue
            isnothing(right_id) && continue
            isnothing(delta) && continue
            # Apply delta: index = right_id * num_left + left_id + 1
            idx = right_id * m.num_left + left_id + 1
            if 1 ≤ idx ≤ length(m.data)
                old = m.data[idx]
                m.data[idx] = Int16(clamp(Int(old) + Int(delta), typemin(Int16), typemax(Int16)))
                count += 1
            end
        end
    end
    @printf(stderr, "  Applied %d matrix patches from %s\n", count, patches_path)
end

# ─── Word Entries ────────────────────────────────────────────────────────────

struct WordEntry
    surface::String
    reading::String
    cost::Int16
    left_id::Int16
    right_id::Int16
    pos_str::String
    char_count::Int
end

"""
    apply_kkc_adjustments(entries) → Dict{String, Vector{WordEntry}}

Apply KKC cost adjustments to raw dictionary entries.
The key insight: reading-keyed Viterbi allows over-segmentation to accumulate
many negative connection costs. We counter this with aggressive cost adjustments:

1. Single-char content words → heavy penalty (prevents over-segmentation)
2. Multi-char words → length bonus (prefers compound words)
3. Hiragana-identity content words → penalty (prefers kanji conversion)
4. Latin-surface entries → extreme penalty
5. Functional words (particles/auxiliary) → left as-is
"""
function apply_kkc_adjustments!(dict::Dict{String, Vector{WordEntry}})
    adjusted = 0
    for (reading, entries) in dict
        n_group = length(entries)
        # Sort by cost for frequency ranking
        sort!(entries, by=e -> e.cost)
        has_kanji = any(e -> !is_all_hiragana(e.surface) && !is_all_latin(e.surface) && !is_all_kana(e.surface), entries)
        reading_len = length(collect(reading))

        for (rank, e) in enumerate(entries)
            cost = Int(e.cost)
            primary_pos = split(e.pos_str, '-')[1]
            is_content = is_content_pos(e.pos_str)
            is_functional = is_functional_pos(e.pos_str)

            # δ_singlechar: heavy penalty for single-char content words
            # These almost never appear in isolation during KKC
            if e.char_count == 1 && is_content
                cost += 15000
            end

            # δ_length: bonus for multi-char words (counters connection cost advantage of over-segmentation)
            # Only for content words; functional words don't benefit from being long
            if e.char_count >= 2 && is_content
                cost -= 3000 * (e.char_count - 1)
            end

            # δ_identity: penalty for hiragana-surface content words when kanji exists
            if is_all_hiragana(e.surface) && is_content && has_kanji
                cost += 5000
            end

            # δ_script: Latin surface with kana reading → extreme penalty
            if is_all_latin(e.surface)
                cost += 20000
            end

            # δ_katakana: katakana surface for non-katakana reading → penalty
            # (prevents デス for です, ハイ for はい)
            if is_all_kana(e.surface) && !is_all_hiragana(e.surface) && is_all_hiragana(reading)
                cost += 3000
            end

            # δ_pos: POS adjustment
            if primary_pos == "名詞"
                cost -= 500
            elseif primary_pos == "動詞"
                cost -= 300
            elseif primary_pos == "形容詞"
                cost -= 200
            elseif primary_pos == "助詞" || primary_pos == "助動詞"
                # Keep functional words relatively cheap so they win at their positions
                cost -= 0
            elseif primary_pos == "接尾辞" || primary_pos == "接頭辞"
                cost += 2000
            elseif primary_pos == "記号" || primary_pos == "補助記号"
                cost += 5000
            end

            # δ_freq: within reading group, boost top entries
            if n_group > 1
                rank_ratio = (rank - 1) / (n_group - 1)
                cost += round(Int, -500 + 800 * rank_ratio)
            else
                cost -= 300  # unique reading bonus
            end

            # Cost floor
            cost = max(cost, 100)
            # Clamp to i16
            cost = clamp(cost, typemin(Int16), typemax(Int16))

            entries[rank] = WordEntry(e.surface, e.reading, Int16(cost), e.left_id, e.right_id, e.pos_str, e.char_count)
            adjusted += 1
        end
    end
    @printf(stderr, "  Applied KKC adjustments to %d entries\n", adjusted)
end

"""
    load_word_dict(csv_path; adjust=false) → Dict{String, Vector{WordEntry}}

Load word entries from CSV, grouped by hiragana reading.
If adjust=true, apply KKC cost adjustments.
"""
function load_word_dict(csv_path::String; adjust::Bool=false)
    dict = Dict{String, Vector{WordEntry}}()
    n = 0

    open(csv_path, "r") do io
        readline(io)  # skip header
        for line in eachline(io)
            fields = parse_csv_fields(line)
            length(fields) >= 10 || continue

            reading_h  = fields[2]
            surface    = fields[4]
            cost       = tryparse(Int16, fields[5])
            left_id    = tryparse(Int16, fields[6])
            right_id   = tryparse(Int16, fields[7])
            pos_str    = fields[9]
            char_count = tryparse(Int, fields[10])

            isnothing(cost)       && continue
            isnothing(left_id)    && continue
            isnothing(right_id)   && continue
            isnothing(char_count) && continue

            entry = WordEntry(surface, reading_h, cost, left_id, right_id, pos_str, char_count)
            push!(get!(dict, reading_h, WordEntry[]), entry)
            n += 1
        end
    end

    @printf(stderr, "  Loaded %d entries, %d unique readings\n", n, length(dict))

    if adjust
        println(stderr, "  Applying KKC cost adjustments...")
        apply_kkc_adjustments!(dict)
    end

    dict
end

# ─── Viterbi Lattice Search ─────────────────────────────────────────────────

struct ViterbiNode
    total_cost::Int32
    prev_pos::Int         # character position of previous node (0 = BOS)
    prev_right_id::Int16  # right_id of the node at prev_pos
    entry::WordEntry      # the word entry chosen at this node
end

"""
    viterbi_convert(input, word_dict, conn) → (segments, total_cost)

Run Viterbi lattice search on hiragana input to find the lowest-cost
segmentation and surface assignment.

Returns a vector of (reading, surface, cost, pos) tuples.
"""
function viterbi_convert(input::String, word_dict::Dict{String, Vector{WordEntry}}, conn::ConnMatrix;
                        boundary_penalty::Int32=Int32(25000))
    chars = collect(input)
    N = length(chars)

    # dp[pos][right_id] → ViterbiNode
    # pos 0 = BOS, pos N = end of string
    dp = [Dict{Int16, ViterbiNode}() for _ in 0:N]

    # BOS: position 0, right_id = 0, cost = 0
    bos_entry = WordEntry("BOS", "", Int16(0), BOS_EOS_ID, BOS_EOS_ID, "BOS", 0)
    dp[1][BOS_EOS_ID] = ViterbiNode(Int32(0), 0, BOS_EOS_ID, bos_entry)

    for i in 0:N-1
        states = dp[i+1]  # +1 for Julia indexing
        isempty(states) && continue

        for (right_id, state) in states
            # Try all possible word lengths starting at position i
            for len in 1:N-i
                reading = String(chars[i+1:i+len])
                entries = get(word_dict, reading, nothing)
                isnothing(entries) && continue

                j = i + len  # end position
                for entry in entries
                    cc = conn_cost(conn, right_id, entry.left_id)
                    # boundary_penalty counters over-segmentation from negative connection costs
                    total = state.total_cost + cc + Int32(entry.cost) + boundary_penalty

                    # Update dp[j+1] if this path is better
                    existing = get(dp[j+1], entry.right_id, nothing)
                    if isnothing(existing) || total < existing.total_cost
                        dp[j+1][entry.right_id] = ViterbiNode(total, i, right_id, entry)
                    end
                end
            end

            # OOV fallback: single character with high cost if nothing matched at this position
            has_any_match = any(haskey(word_dict, String(chars[i+1:i+len])) for len in 1:N-i)
            if !has_any_match
                c = chars[i+1]
                oov_entry = WordEntry(string(c), string(c), Int16(OOV_COST), BOS_EOS_ID, BOS_EOS_ID, "OOV", 1)
                cc = conn_cost(conn, right_id, BOS_EOS_ID)
                total = state.total_cost + cc + OOV_COST + boundary_penalty
                j = i + 1
                existing = get(dp[j+1], BOS_EOS_ID, nothing)
                if isnothing(existing) || total < existing.total_cost
                    dp[j+1][BOS_EOS_ID] = ViterbiNode(total, i, right_id, oov_entry)
                end
            end
        end
    end

    # Find best EOS connection
    best_cost = typemax(Int32)
    best_right_id = BOS_EOS_ID
    eos_states = dp[N+1]

    for (right_id, state) in eos_states
        eos_cc = conn_cost(conn, right_id, BOS_EOS_ID)
        total = state.total_cost + eos_cc
        if total < best_cost
            best_cost = total
            best_right_id = right_id
        end
    end

    if best_cost == typemax(Int32)
        println(stderr, "  WARNING: No valid path found!")
        return (Tuple{String,String,Int16,String}[], Int32(0))
    end

    # Backtrack to recover path
    segments = Tuple{String,String,Int16,String}[]
    pos = N
    rid = best_right_id

    while pos > 0
        node = dp[pos+1][rid]
        reading = String(chars[node.prev_pos+1:pos])
        pushfirst!(segments, (reading, node.entry.surface, node.entry.cost, node.entry.pos_str))
        rid = node.prev_right_id
        pos = node.prev_pos
    end

    (segments, best_cost)
end

# ─── Test Runner ─────────────────────────────────────────────────────────────

struct TestCase
    input::String
    expected::String
    description::String
end

const TEST_CASES = [
    TestCase("きょうはいいてんきですね", "今日はいい天気ですね", "basic KKC sentence"),
]

function run_tests(word_dict, conn)
    passed = 0
    failed = 0

    for tc in TEST_CASES
        segments, total_cost = viterbi_convert(tc.input, word_dict, conn)
        result = join(s[2] for s in segments)

        ok = result == tc.expected
        status = ok ? "✓ PASS" : "✗ FAIL"
        ok ? (passed += 1) : (failed += 1)

        println()
        println("─── $(tc.description) ───")
        @printf("  Input:    %s\n", tc.input)
        @printf("  Expected: %s\n", tc.expected)
        @printf("  Got:      %s  %s\n", result, status)
        @printf("  Cost:     %d\n", total_cost)
        println("  Segmentation:")
        for (reading, surface, cost, pos) in segments
            primary_pos = split(pos, '-')[1]
            @printf("    %s → %s  (cost=%d, POS=%s)\n", reading, surface, cost, primary_pos)
        end
    end

    println()
    println("═══════════════════════════════════════")
    @printf("  Results: %d passed, %d failed / %d total\n", passed, failed, passed + failed)
    println("═══════════════════════════════════════")

    return failed == 0
end

# ─── Debug: show top candidates for each reading in test input ───────────────

function debug_candidates(input::String, word_dict::Dict{String, Vector{WordEntry}})
    chars = collect(input)
    N = length(chars)

    println("\n── Candidate analysis for: $input ──")
    seen_readings = Set{String}()

    for i in 0:N-1
        for len in 1:min(6, N-i)
            reading = String(chars[i+1:i+len])
            reading ∈ seen_readings && continue
            entries = get(word_dict, reading, nothing)
            isnothing(entries) && continue
            push!(seen_readings, reading)

            sorted = sort(entries, by=e->e.cost)
            top = sorted[1:min(5, length(sorted))]
            @printf("  [%d:%d] \"%s\" (%d candidates):\n", i, i+len, reading, length(sorted))
            for e in top
                primary_pos = split(e.pos_str, '-')[1]
                @printf("    %s  cost=%d  POS=%s  L=%d R=%d\n",
                    e.surface, e.cost, primary_pos, e.left_id, e.right_id)
            end
        end
    end
end

# ─── Main ────────────────────────────────────────────────────────────────────

function main()
    if length(ARGS) < 2
        println(stderr, "Usage: julia kkc_test.jl <system.dic> <csv> [--adjust] [matrix_patches.csv]")
        println(stderr, "")
        println(stderr, "Tests KKC conversion quality using Viterbi lattice search.")
        println(stderr, "  --adjust  Apply KKC cost adjustments inline (for raw export CSV)")
        println(stderr, "  Without --adjust, assumes CSV already has adjusted costs.")
        println(stderr, "")
        println(stderr, "Typical tuning loop:")
        println(stderr, "  # With inline adjustment (tune kkc_test.jl directly):")
        println(stderr, "  julia julia/kkc_test.jl sudachi.rs/resources/system.dic /tmp/kkc_tune/words_export.csv --adjust")
        println(stderr, "  # With pre-adjusted CSV:")
        println(stderr, "  julia julia/kkc_costs.jl /tmp/kkc_tune/words_export.csv /tmp/kkc_tune")
        println(stderr, "  julia julia/kkc_test.jl sudachi.rs/resources/system.dic /tmp/kkc_tune/adjusted.csv")
        exit(1)
    end

    dic_path = ARGS[1]
    csv_path = ARGS[2]
    do_adjust = "--adjust" ∈ ARGS
    patches_path = nothing
    for a in ARGS[3:end]
        if a != "--adjust" && isfile(a)
            patches_path = a
        end
    end

    println(stderr, "Loading connection matrix from $dic_path...")
    conn = load_connection_matrix(dic_path)

    if !isnothing(patches_path)
        println(stderr, "Loading matrix patches...")
        apply_matrix_patches!(conn, patches_path)
    end

    println(stderr, "Loading word entries from $csv_path...")
    word_dict = load_word_dict(csv_path; adjust=do_adjust)

    # Debug: show candidates for test inputs
    for tc in TEST_CASES
        debug_candidates(tc.input, word_dict)
    end

    # Run tests
    all_passed = run_tests(word_dict, conn)

    exit(all_passed ? 0 : 1)
end

main()
