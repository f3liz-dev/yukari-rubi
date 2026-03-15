"""
    Matrix Patch Generator for KKC

Generates connection matrix patches based on POS-level rules.
Reads the word export CSV to build left_id/right_id → POS mappings,
then applies linguistic rules to produce (left_id, right_id, delta) patches.

The output `matrix_patches.csv` is consumed by `dic_converter --matrix-patches`.

# Connection Rules

IME-specific adjustments to the Sudachi connection matrix:

1. **Verb stem → negative ない**: boost (more natural IME flow)
2. **Verb stem → ます/ました**: boost (polite form connections)
3. **Noun → です/だ**: boost (copula connections)
4. **Noun → の/が/を/は (particles)**: boost
5. **Latin noun → auxiliary (です etc)**: penalize (prevents DEATH+です)
6. **Adjective stem → い/く/かった**: boost (adjective inflections)

Usage:
    julia matrix_patches.jl <export.csv> <output_patches.csv>

The export CSV is the same one produced by `kkc_builder --export`.
"""

using Printf

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

# ─── Script Detection ────────────────────────────────────────────────────────

is_latin_char(c::Char) = 'A' ≤ c ≤ 'Z' || 'a' ≤ c ≤ 'z' || 'Ａ' ≤ c ≤ 'Ｚ' || 'ａ' ≤ c ≤ 'ｚ'
is_all_latin(s::AbstractString) = !isempty(s) && all(is_latin_char, s)

# ─── POS Mapping Builder ────────────────────────────────────────────────────

struct EntryInfo
    left_id::Int16
    right_id::Int16
    pos_str::String
    surface::String
end

"""
    build_id_pos_maps(csv_path) → (left_id_to_pos, right_id_to_pos)

Build mappings from left_id/right_id to the set of POS categories
that use each ID. This lets us define matrix patches by POS rather
than by raw ID numbers.
"""
function build_id_pos_maps(csv_path::String)
    left_id_to_pos = Dict{Int16, Set{String}}()
    right_id_to_pos = Dict{Int16, Set{String}}()
    left_id_has_latin = Dict{Int16, Bool}()

    entries = EntryInfo[]

    open(csv_path, "r") do io
        readline(io)  # skip header

        for line in eachline(io)
            fields = parse_csv_fields(line)
            length(fields) >= 10 || continue

            left_id  = tryparse(Int16, fields[6])
            right_id = tryparse(Int16, fields[7])
            pos_str  = fields[9]
            surface  = fields[4]

            isnothing(left_id)  && continue
            isnothing(right_id) && continue

            primary_pos = split(pos_str, '-')[1]

            push!(get!(left_id_to_pos, left_id, Set{String}()), primary_pos)
            push!(get!(right_id_to_pos, right_id, Set{String}()), primary_pos)

            # Track which left_ids are associated with Latin surfaces
            if is_all_latin(surface)
                left_id_has_latin[left_id] = true
            end

            push!(entries, EntryInfo(left_id, right_id, pos_str, surface))
        end
    end

    (left_id_to_pos, right_id_to_pos, left_id_has_latin, entries)
end

# ─── Patch Rule Definitions ─────────────────────────────────────────────────

# Each rule: (description, right_id_pos_filter, left_id_pos_filter, delta)
# right_id = the preceding word's connection point
# left_id  = the following word's connection point
# delta    = cost adjustment (negative = boost, positive = penalize)

struct PatchRule
    name::String
    right_pos_match::Function    # does the right_id POS set match?
    left_pos_match::Function     # does the left_id POS set match?
    delta::Int16
    extra_filter::Function       # additional filter on (left_id, right_id)
end

const DEFAULT_FILTER = (_, _) -> true

function define_patch_rules(left_id_has_latin::Dict{Int16, Bool})
    [
        # 動詞 → 助動詞（ない、ます、た、etc.）: boost natural verb inflections
        PatchRule(
            "動詞→助動詞 boost",
            pos -> "動詞" ∈ pos,
            pos -> "助動詞" ∈ pos,
            Int16(-300),
            DEFAULT_FILTER,
        ),

        # 名詞 → 助動詞（です、だ）: boost copula connections
        PatchRule(
            "名詞→助動詞 boost",
            pos -> "名詞" ∈ pos,
            pos -> "助動詞" ∈ pos,
            Int16(-200),
            DEFAULT_FILTER,
        ),

        # 名詞 → 助詞（の、が、を、は）: boost particle connections
        PatchRule(
            "名詞→助詞 boost",
            pos -> "名詞" ∈ pos,
            pos -> "助詞" ∈ pos,
            Int16(-150),
            DEFAULT_FILTER,
        ),

        # 形容詞 → 助動詞: boost adjective inflection connections
        PatchRule(
            "形容詞→助動詞 boost",
            pos -> "形容詞" ∈ pos,
            pos -> "助動詞" ∈ pos,
            Int16(-200),
            DEFAULT_FILTER,
        ),

        # Latin-surface left_ids → 助動詞: penalize (DEATH+です problem)
        PatchRule(
            "Latin→助動詞 penalty",
            pos -> "名詞" ∈ pos,
            pos -> "助動詞" ∈ pos,
            Int16(500),
            (left_id, _) -> get(left_id_has_latin, left_id, false),
        ),

        # 動詞 → 助詞（て、で）: boost te-form connections
        PatchRule(
            "動詞→助詞 boost",
            pos -> "動詞" ∈ pos,
            pos -> "助詞" ∈ pos,
            Int16(-100),
            DEFAULT_FILTER,
        ),

        # 副詞 → 動詞: boost (adverb before verb is natural)
        PatchRule(
            "副詞→動詞 boost",
            pos -> "副詞" ∈ pos,
            pos -> "動詞" ∈ pos,
            Int16(-100),
            DEFAULT_FILTER,
        ),
    ]
end

# ─── Patch Generation ────────────────────────────────────────────────────────

"""
    generate_patches(left_id_to_pos, right_id_to_pos, rules) → patches

Generate (left_id, right_id, delta) tuples from POS-based rules.
"""
function generate_patches(
    left_id_to_pos::Dict{Int16, Set{String}},
    right_id_to_pos::Dict{Int16, Set{String}},
    left_id_has_latin::Dict{Int16, Bool},
    rules::Vector{PatchRule},
)
    # Accumulate patches: (left_id, right_id) → total delta
    patch_map = Dict{Tuple{Int16, Int16}, Int}()

    for rule in rules
        matched = 0
        # Find all right_ids that match the rule's right POS filter
        matching_right_ids = [rid for (rid, pos) in right_id_to_pos if rule.right_pos_match(pos)]
        # Find all left_ids that match the rule's left POS filter
        matching_left_ids = [lid for (lid, pos) in left_id_to_pos if rule.left_pos_match(pos)]

        for rid in matching_right_ids
            for lid in matching_left_ids
                if rule.extra_filter(lid, rid)
                    key = (lid, rid)
                    patch_map[key] = get(patch_map, key, 0) + Int(rule.delta)
                    matched += 1
                end
            end
        end

        @printf(stderr, "  Rule %-24s: %6d patches (right_ids: %d, left_ids: %d)\n",
            rule.name, matched, length(matching_right_ids), length(matching_left_ids))
    end

    patch_map
end

# ─── Output ──────────────────────────────────────────────────────────────────

function write_patches_csv(path::String, patches::Dict{Tuple{Int16, Int16}, Int})
    open(path, "w") do io
        println(io, "left_id,right_id,delta")
        for ((lid, rid), delta) in sort(collect(patches), by=x -> (x[1][1], x[1][2]))
            # Clamp delta to i16 range
            clamped = Int16(clamp(delta, typemin(Int16), typemax(Int16)))
            println(io, "$lid,$rid,$clamped")
        end
    end
end

# ─── Main ────────────────────────────────────────────────────────────────────

function main()
    if length(ARGS) < 2
        println(stderr, "Usage: julia matrix_patches.jl <export.csv> <output_patches.csv>")
        println(stderr, "")
        println(stderr, "Generates connection matrix patches from POS-based rules.")
        println(stderr, "The export CSV is from `kkc_builder --export`.")
        exit(1)
    end

    export_csv = ARGS[1]
    output_path = ARGS[2]

    println(stderr, "Building ID → POS mappings from $export_csv...")
    left_id_to_pos, right_id_to_pos, left_id_has_latin, entries = build_id_pos_maps(export_csv)
    println(stderr, "  $(length(left_id_to_pos)) unique left_ids")
    println(stderr, "  $(length(right_id_to_pos)) unique right_ids")
    println(stderr, "  $(sum(values(left_id_has_latin))) left_ids with Latin surfaces")

    println(stderr, "\nApplying patch rules...")
    rules = define_patch_rules(left_id_has_latin)
    patches = generate_patches(left_id_to_pos, right_id_to_pos, left_id_has_latin, rules)

    println(stderr, "\nTotal: $(length(patches)) unique (left_id, right_id) patches")

    write_patches_csv(output_path, patches)
    println(stderr, "Written to $output_path")
end

main()
