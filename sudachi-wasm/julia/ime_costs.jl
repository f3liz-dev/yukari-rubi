"""
    IME Cost Parameter Optimization

Pre-compute optimal α (compound boost) and β (identity penalty) parameters
for the Kana-Kanji Conversion lattice Viterbi search.

# Cost Model

    E_IME(w) = E_orig(w) - α·𝟙(w ∈ Mode C) + β·𝟙(w ∈ Mode A)

where:
- E_orig(w)  = original Sudachi cost (left_id, right_id, cost triplet)
- Mode C     = compound/multi-char entries (span_len > 1)
- Mode A     = single-char identity/passthrough nodes
- α          = per-extra-character discount for longer spans
- β          = penalty for identity nodes to prefer dictionary entries

In practice:
    adjusted_cost = cost - α × (span_chars - 1)    for dictionary entries
    identity_cost = β                                for passthrough nodes
"""

using Statistics
using Printf

# ─── Connection Matrix Analysis ──────────────────────────────────────────────

"""
    load_connection_matrix(path::String) → Matrix{Int16}

Load a Sudachi connection matrix (matrix.def format or raw binary).
Returns the cost matrix indexed as M[right+1, left+1] (1-based).
"""
function load_connection_matrix(path::String)
    lines = readlines(path)
    header = split(lines[1])
    num_left = parse(Int, header[1])
    num_right = parse(Int, header[2])

    M = zeros(Int16, num_right, num_left)
    for line in lines[2:end]
        parts = split(line)
        length(parts) >= 3 || continue
        left = parse(Int, parts[1]) + 1
        right = parse(Int, parts[2]) + 1
        cost = parse(Int16, parts[3])
        M[right, left] = cost
    end
    M
end

"""
    analyze_connection_costs(M::Matrix{Int16})

Analyze the connection cost matrix to derive statistics useful for
parameter tuning.

Returns a NamedTuple with:
- `μ_conn`: mean connection cost
- `σ_conn`: std of connection costs
- `median_conn`: median connection cost
- `p25`, `p75`: quartiles
- `nonzero_frac`: fraction of nonzero entries
"""
function analyze_connection_costs(M::Matrix{Int16})
    vals = vec(M)
    nonzero = filter(!iszero, vals)

    (
        μ_conn = mean(Float64.(vals)),
        σ_conn = std(Float64.(vals)),
        median_conn = median(Float64.(nonzero)),
        p25 = quantile(Float64.(nonzero), 0.25),
        p75 = quantile(Float64.(nonzero), 0.75),
        nonzero_frac = length(nonzero) / length(vals),
    )
end

# ─── Word Cost Analysis ─────────────────────────────────────────────────────

"""
    WordEntry

Minimal word entry for cost analysis.
"""
struct WordEntry
    surface::String
    reading::String
    cost::Int16
    left_id::Int16
    right_id::Int16
    span_chars::Int
end

"""
    load_word_costs(path::String) → Vector{WordEntry}

Load word entries from a TSV dictionary file (Sudachi CSV format).
Expected columns: surface, left_id, right_id, cost, ..., reading_form
"""
function load_word_costs(path::String)
    entries = WordEntry[]
    for line in eachline(path)
        parts = split(line, ',')
        length(parts) >= 12 || continue

        surface = parts[1]
        left_id = tryparse(Int16, parts[2])
        right_id = tryparse(Int16, parts[3])
        cost = tryparse(Int16, parts[4])
        reading = parts[12]

        isnothing(left_id) && continue
        isnothing(right_id) && continue
        isnothing(cost) && continue

        push!(entries, WordEntry(
            surface, reading, cost, left_id, right_id,
            length(surface),
        ))
    end
    entries
end

# ─── α Parameter Derivation ─────────────────────────────────────────────────

"""
    derive_α(entries::Vector{WordEntry}; target_ratio=0.7) → Int

Derive the compound boost parameter α.

Strategy: analyze cost distributions by span length. α should be set such
that a 2-char compound entry is preferred over two 1-char entries when the
compound's cost is within `target_ratio` of the sum of the singles' costs.

    α = median_cost_per_char_reduction

where cost_per_char_reduction is how much cost decreases per additional
character in the span, measured across the dictionary.
"""
function derive_α(entries::Vector{WordEntry}; target_ratio=0.7)
    by_len = Dict{Int, Vector{Int16}}()
    for e in entries
        e.span_chars >= 1 || continue
        costs = get!(by_len, e.span_chars, Int16[])
        push!(costs, e.cost)
    end

    # Compute median cost per span length
    medians = Dict{Int, Float64}()
    for (len, costs) in by_len
        length(costs) >= 10 || continue
        medians[len] = median(Float64.(costs))
    end

    if !haskey(medians, 1) || !haskey(medians, 2)
        @warn "Insufficient data for α derivation, using default"
        return 500
    end

    # Cost per character: how does median cost change with span length?
    # For span_len=1: cost₁ (base cost per character)
    # For span_len=2: cost₂ (two chars in one entry)
    # If two singles cost 2×cost₁, the compound saves (2×cost₁ - cost₂)
    # α should make: cost₂ - α ≈ target_ratio × (2 × cost₁)
    # → α ≈ cost₂ - target_ratio × 2 × cost₁

    cost_reduction = medians[1] - (medians[2] / 2)
    α = max(100, round(Int, abs(cost_reduction) * target_ratio))

    @info "α derivation" medians[1] medians[2] cost_reduction α
    α
end

# ─── β Parameter Derivation ─────────────────────────────────────────────────

"""
    derive_β(entries::Vector{WordEntry}; percentile=0.95) → Int

Derive the identity penalty parameter β.

Strategy: β should be high enough that identity (passthrough) nodes are
only chosen when no dictionary entry exists. Set β above the `percentile`
of single-character entry costs, ensuring dictionary entries are always
preferred over passthrough.

    β = P_{percentile}(costs of 1-char entries) × safety_factor
"""
function derive_β(entries::Vector{WordEntry}; percentile=0.95, safety_factor=1.5)
    single_costs = Float64[e.cost for e in entries if e.span_chars == 1]

    if isempty(single_costs)
        @warn "No single-char entries found, using default β"
        return 12000
    end

    p = quantile(single_costs, percentile)
    β = round(Int, p * safety_factor)

    @info "β derivation" percentile p safety_factor β
    β
end

# ─── Visualization ───────────────────────────────────────────────────────────

"""
    cost_distribution_report(entries::Vector{WordEntry})

Print a report of cost distributions by span length for parameter tuning.
"""
function cost_distribution_report(entries::Vector{WordEntry})
    by_len = Dict{Int, Vector{Float64}}()
    for e in entries
        costs = get!(by_len, e.span_chars, Float64[])
        push!(costs, Float64(e.cost))
    end

    println("┌─────────┬──────────┬──────────┬──────────┬──────────┬──────────┐")
    println("│ Span    │ Count    │ Mean     │ Median   │ P5       │ P95      │")
    println("├─────────┼──────────┼──────────┼──────────┼──────────┼──────────┤")

    for len in sort(collect(keys(by_len)))
        costs = by_len[len]
        length(costs) >= 5 || continue
        Printf.@printf("│ %3d     │ %8d │ %8.1f │ %8.1f │ %8.1f │ %8.1f │\n",
            len, length(costs), mean(costs), median(costs),
            quantile(costs, 0.05), quantile(costs, 0.95))
    end
    println("└─────────┴──────────┴──────────┴──────────┴──────────┴──────────┘")
end

"""
    optimize_parameters(dict_path::String; connection_matrix_path=nothing)

Run the full parameter optimization pipeline.

Returns (α, β) tuple suitable for use in conversion.rs.
"""
function optimize_parameters(dict_path::String; connection_matrix_path=nothing)
    @info "Loading dictionary entries from $dict_path"
    entries = load_word_costs(dict_path)
    @info "Loaded $(length(entries)) entries"

    cost_distribution_report(entries)

    α = derive_α(entries)
    β = derive_β(entries)

    if !isnothing(connection_matrix_path)
        @info "Analyzing connection matrix"
        M = load_connection_matrix(connection_matrix_path)
        stats = analyze_connection_costs(M)
        @info "Connection matrix stats" stats
    end

    println("\n═══════════════════════════════════════════")
    println("  Optimized IME Cost Parameters")
    println("═══════════════════════════════════════════")
    println("  α (IME_COMPOUND_BOOST) = $α")
    println("  β (IME_IDENTITY_COST)  = $β")
    println("═══════════════════════════════════════════")
    println("\nRust constants for conversion.rs:")
    println("  const IME_COMPOUND_BOOST: i16 = $α;")
    println("  const IME_IDENTITY_COST: i16 = $β;")

    (α=α, β=β)
end
