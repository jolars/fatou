#!/usr/bin/env julia
#
# generate-logo.jl — generate the Fatou logo from a filled Julia set.
#
# Fatou and Julia are two halves of the same picture in complex dynamics: for
# the map z ↦ z² + c, the plane splits into the Julia set (the chaotic
# boundary) and the Fatou set (the stable basins — everything else). This tool
# parses the Julia *language*; the brand is *Fatou*, the calm complement. So the
# logo is a filled Julia set, drawn in the Julia brand colors.
#
# Self-contained: no package dependencies. Emits an antialiased RGBA PNG (via a
# hand-rolled encoder) and a true vector SVG (via marching squares).
#
# Usage:
#   julia scripts/generate-logo.jl
#   julia scripts/generate-logo.jl c=-0.123,0.745 extent=1.45 out=assets/fatou
#   julia scripts/generate-logo.jl preset=dendrite
#
# Parameters (key=value):
#   preset    rabbit | sanmarco | dendrite | airplane   (sets c, overridable)
#   c         "re,im"          complex parameter        (default rabbit)
#   center    "re,im"          view center              (default 0,0)
#   extent    Float            half-width of the view   (default 1.45)
#   maxiter   Int              escape iterations        (default 250)
#   png       Int              PNG size in px           (default 720)
#   ss        Int              supersampling factor     (default 3)
#   grid      Int              SVG marching-squares res (default 640)
#   smooth    Float            simplify: opening radius as fraction of width,
#                              e.g. 0.02 drops the boundary filigree (default 0)
#   border    Float            newton: black frame as fraction of width
#                              (default 0.025 in newton mode, else 0)
#   out       String           output prefix            (default assets/fatou-logo)

# --- Julia brand colors -----------------------------------------------------

const JULIA_PURPLE = (0x95, 0x58, 0xB2)
const JULIA_BLUE = (0x40, 0x63, 0xD8)
const JULIA_GREEN = (0x38, 0x98, 0x26)
const JULIA_RED = (0xCB, 0x3C, 0x33)

# Gradient stops swept across the fill, position in [0, 1].
const GRADIENT = [
    (0.0, JULIA_PURPLE),
    (0.34, JULIA_BLUE),
    (0.67, JULIA_GREEN),
    (1.0, JULIA_RED),
]

# Newton basins for z³ - 1, one Julia-dot color per cube root of unity.
const NEWTON_COLORS = (JULIA_RED, JULIA_GREEN, JULIA_PURPLE)

const PRESETS = Dict(
    "rabbit" => complex(-0.093, 0.705),  # Douady rabbit — three-lobed, organic
    "sanmarco" => complex(-0.85, 0.0),     # San Marco — symmetric, classic
    "dendrite" => complex(0.0, 1),       # dendrite — thin, branchy
    "airplane" => complex(-1.7548, 0.0),   # airplane — spiky
)

# --- argument parsing -------------------------------------------------------

function getarg(args, key, default)
    for a in args
        startswith(a, key * "=") && return split(a, "=", limit = 2)[2]
    end
    return default
end

parsecomplex(s) = (p = parse.(Float64, split(s, ",")); complex(p[1], p[2]))

# --- color helpers ----------------------------------------------------------

function gradient_color(t)
    t = clamp(t, 0.0, 1.0)
    for k in 1:(length(GRADIENT) - 1)
        p0, c0 = GRADIENT[k]
        p1, c1 = GRADIENT[k + 1]
        if t <= p1
            f = (t - p0) / (p1 - p0)
            return ntuple(d -> round(UInt8, Float64(c0[d]) + f * (Float64(c1[d]) - Float64(c0[d]))), 3)
        end
    end
    return GRADIENT[end][2]
end

# --- fractal ----------------------------------------------------------------

# True if z₀ never escapes |z| > R under z ↦ z² + c (i.e. in the filled set).
@inline function in_set(z0::ComplexF64, c::ComplexF64, maxiter::Int, R2::Float64)
    z = z0
    @inbounds for _ in 1:maxiter
        z = z * z + c
        abs2(z) > R2 && return false
    end
    return true
end

# Map pixel (row i from top, col j) to the complex plane. Row 0 = top = +im.
@inline function to_complex(i, j, w, h, cx, cy, extent)
    re = cx + ((j - 0.5) / w * 2 - 1) * extent
    im = cy + (1 - (i - 0.5) / h * 2) * extent
    return complex(re, im)
end

# --- geometry simplification (morphology) -----------------------------------

# Offsets of a filled disk of radius r (Euclidean), for structuring elements.
function disk_offsets(r::Int)
    offs = Tuple{Int, Int}[]
    r <= 0 && return offs
    r2 = r * r
    for di in (-r):r, dj in (-r):r
        di * di + dj * dj <= r2 && push!(offs, (di, dj))
    end
    return offs
end

# Erosion: a cell survives only if every disk offset around it is set
# (out-of-bounds counts as unset). Shrinks the set, severs thin filaments.
function erode(mask::AbstractMatrix{Bool}, offs)
    isempty(offs) && return copy(mask)
    h, w = size(mask)
    out = falses(h, w)
    Threads.@threads for i in 1:h
        @inbounds for j in 1:w
            keep = true
            for (di, dj) in offs
                ii = i + di
                jj = j + dj
                if ii < 1 || ii > h || jj < 1 || jj > w || !mask[ii, jj]
                    keep = false
                    break
                end
            end
            out[i, j] = keep
        end
    end
    return out
end

# Dilation: a cell is set if any disk offset around it is set. Regrows the set.
function dilate(mask::AbstractMatrix{Bool}, offs)
    isempty(offs) && return copy(mask)
    h, w = size(mask)
    out = falses(h, w)
    Threads.@threads for i in 1:h
        @inbounds for j in 1:w
            hit = false
            for (di, dj) in offs
                ii = i + di
                jj = j + dj
                if 1 <= ii <= h && 1 <= jj <= w && mask[ii, jj]
                    hit = true
                    break
                end
            end
            out[i, j] = hit
        end
    end
    return out
end

# Simplify the mask: opening (erode→dilate) drops features thinner than the
# filigree; closing (dilate→erode) afterward fills pinholes. `frac` is the
# structuring-element radius as a fraction of the mask width, so the look is
# the same across render resolutions.
function simplify_mask(mask::AbstractMatrix{Bool}, frac::Float64)
    frac <= 0 && return mask
    r = max(1, round(Int, frac * size(mask, 2)))
    offs = disk_offsets(r)
    opened = dilate(erode(mask, offs), offs)
    return erode(dilate(opened, offs), offs)
end

# --- PNG encoder (zero dependencies) ----------------------------------------

const CRC_TABLE = let tbl = Vector{UInt32}(undef, 256)
    for n in 0:255
        c = UInt32(n)
        for _ in 1:8
            c = (c & 0x01) != 0 ? (0xedb88320 ⊻ (c >> 1)) : (c >> 1)
        end
        tbl[n + 1] = c
    end
    tbl
end

function crc32(data)
    c = 0xffffffff
    @inbounds for b in data
        c = CRC_TABLE[Int((c ⊻ b) & 0xff) + 1] ⊻ (c >> 8)
    end
    return c ⊻ 0xffffffff
end

function adler32(data)
    a = UInt32(1)
    b = UInt32(0)
    @inbounds for byte in data
        a = (a + byte) % 65521
        b = (b + a) % 65521
    end
    return (b << 16) | a
end

# zlib stream with stored (uncompressed) DEFLATE blocks — no compressor needed.
function zlib_stored(data::Vector{UInt8})
    out = UInt8[0x78, 0x01]
    n = length(data)
    pos = 1
    while pos <= n
        blocklen = min(n - pos + 1, 65535)
        final = (pos + blocklen - 1) >= n ? 0x01 : 0x00
        push!(out, final)
        push!(out, UInt8(blocklen & 0xff), UInt8((blocklen >> 8) & 0xff))
        nlen = ~UInt16(blocklen)
        push!(out, UInt8(nlen & 0xff), UInt8((nlen >> 8) & 0xff))
        append!(out, @view data[pos:(pos + blocklen - 1)])
        pos += blocklen
    end
    ad = adler32(data)
    push!(
        out, UInt8((ad >> 24) & 0xff), UInt8((ad >> 16) & 0xff),
        UInt8((ad >> 8) & 0xff), UInt8(ad & 0xff)
    )
    return out
end

be32(x) = UInt8[(x >> 24) & 0xff, (x >> 16) & 0xff, (x >> 8) & 0xff, x & 0xff]

function write_chunk(io, ctype::String, data::Vector{UInt8})
    write(io, be32(UInt32(length(data))))
    typ = Vector{UInt8}(codeunits(ctype))
    write(io, typ)
    write(io, data)
    return write(io, be32(crc32(vcat(typ, data))))
end

# img is (height, width, 4) RGBA bytes.
function write_png(path, img::Array{UInt8, 3})
    h, w = size(img, 1), size(img, 2)
    raw = Vector{UInt8}(undef, h * (1 + 4w))
    p = 1
    @inbounds for i in 1:h
        raw[p] = 0x00  # filter: none
        p += 1
        for j in 1:w, k in 1:4
            raw[p] = img[i, j, k]
            p += 1
        end
    end
    return open(path, "w") do io
        write(io, UInt8[137, 80, 78, 71, 13, 10, 26, 10])
        ihdr = vcat(be32(UInt32(w)), be32(UInt32(h)), UInt8[8, 6, 0, 0, 0])
        write_chunk(io, "IHDR", ihdr)
        write_chunk(io, "IDAT", zlib_stored(raw))
        write_chunk(io, "IEND", UInt8[])
    end
end

# --- raster render (supersampled, antialiased) ------------------------------

function render_png(path, c, cx, cy, extent, maxiter, size, ss, smooth)
    R2 = 4.0
    W = size * ss
    H = size * ss
    # Subpixel coverage of the filled set.
    cover = Matrix{Bool}(undef, H, W)
    Threads.@threads for i in 1:H
        @inbounds for j in 1:W
            cover[i, j] = in_set(to_complex(i, j, W, H, cx, cy, extent), c, maxiter, R2)
        end
    end
    cover = simplify_mask(cover, smooth)
    inv = 1.0 / (ss * ss)
    # Per-pixel coverage, plus the filled bounding box so the gradient spans the
    # shape (not the canvas) — matching the SVG's objectBoundingBox gradient and
    # keeping the full color range regardless of how smoothing resizes the shape.
    alphas = zeros(Float64, size, size)
    imin, imax, jmin, jmax = size + 1, 0, size + 1, 0
    @inbounds for bi in 1:size, bj in 1:size
        cnt = 0
        for di in 1:ss, dj in 1:ss
            cover[(bi - 1) * ss + di, (bj - 1) * ss + dj] && (cnt += 1)
        end
        cnt == 0 && continue
        alphas[bi, bj] = cnt * inv
        imin = min(imin, bi); imax = max(imax, bi)
        jmin = min(jmin, bj); jmax = max(jmax, bj)
    end
    img = zeros(UInt8, size, size, 4)
    di = max(imax - imin, 1)
    dj = max(jmax - jmin, 1)
    @inbounds for bi in 1:size, bj in 1:size
        alpha = alphas[bi, bj]
        alpha == 0 && continue
        t = ((bi - imin) / di + (bj - jmin) / dj) / 2  # diagonal across bbox
        col = gradient_color(t)
        img[bi, bj, 1] = col[1]
        img[bi, bj, 2] = col[2]
        img[bi, bj, 3] = col[3]
        img[bi, bj, 4] = round(UInt8, alpha * 255)
    end
    return write_png(path, img)
end

# --- Newton fractal render --------------------------------------------------

# Newton's method for f(z) = z³ - 1: z ↦ z - (z³-1)/(3z²) = (2z³ + 1)/(3z²).
# Every point flows to one of the three cube roots of unity (the Fatou basins);
# the Julia set is the fractal boundary between them, left white. Each basin is
# tinted by its root and lightened toward white near the boundary (slow
# convergence), so the filigree reads as the white Julia set of the reference.
@inline function newton_basin(z::ComplexF64, roots, maxiter::Int, tol2::Float64)
    @inbounds for k in 1:maxiter
        z2 = z * z
        abs2(z2) < 1.0e-18 && return (0, k)  # near the pole at 0
        z = z - (z * z2 - 1) / (3 * z2)
        for r in 1:3
            abs2(z - roots[r]) < tol2 && return (r, k)
        end
    end
    return (0, maxiter)  # undecided → Julia set
end

function render_newton_png(path, cx, cy, extent, maxiter, size, ss, whiten, border)
    roots = (complex(1.0, 0.0), cispi(2 / 3), cispi(4 / 3))
    tol2 = 1.0e-12
    W = size * ss
    H = size * ss
    # Accumulate premultiplied RGB so antialiasing blends basin colors cleanly.
    acc = zeros(Float64, size, size, 3)
    Threads.@threads for bi in 1:size
        @inbounds for bj in 1:size
            r = 0.0
            g = 0.0
            b = 0.0
            for di in 1:ss, dj in 1:ss
                i = (bi - 1) * ss + di
                j = (bj - 1) * ss + dj
                root, k = newton_basin(to_complex(i, j, W, H, cx, cy, extent), roots, maxiter, tol2)
                if root == 0
                    r += 255; g += 255; b += 255  # Julia set: white
                else
                    base = NEWTON_COLORS[root]
                    f = clamp((k / maxiter)^2.0 * whiten, 0.0, 1.0)  # lighten near boundary
                    r += base[1] + f * (255 - base[1])
                    g += base[2] + f * (255 - base[2])
                    b += base[3] + f * (255 - base[3])
                end
            end
            acc[bi, bj, 1] = r
            acc[bi, bj, 2] = g
            acc[bi, bj, 3] = b
        end
    end
    img = zeros(UInt8, size, size, 4)
    inv = 1.0 / (ss * ss)
    @inbounds for bi in 1:size, bj in 1:size
        img[bi, bj, 1] = round(UInt8, acc[bi, bj, 1] * inv)
        img[bi, bj, 2] = round(UInt8, acc[bi, bj, 2] * inv)
        img[bi, bj, 3] = round(UInt8, acc[bi, bj, 3] * inv)
        img[bi, bj, 4] = 0xff
    end
    # Paint an opaque black frame over the perimeter (full-bleed render).
    bpx = max(0, round(Int, border * size))
    if bpx > 0
        @inbounds for bi in 1:size, bj in 1:size
            if bi <= bpx || bi > size - bpx || bj <= bpx || bj > size - bpx
                img[bi, bj, 1] = 0
                img[bi, bj, 2] = 0
                img[bi, bj, 3] = 0
                img[bi, bj, 4] = 0xff
            end
        end
    end
    return write_png(path, img)
end

# --- vector render (marching squares) ---------------------------------------

function set_mask(c, cx, cy, extent, maxiter, g, smooth)
    R2 = 4.0
    mask = falses(g, g)
    Threads.@threads for i in 1:g
        @inbounds for j in 1:g
            mask[i, j] = in_set(to_complex(i, j, g, g, cx, cy, extent), c, maxiter, R2)
        end
    end
    return simplify_mask(mask, smooth)
end

# Trace the iso-contour of `mask` into closed loops of (x, y) points.
function contour_loops(mask)
    g = size(mask, 1)
    # Crossing points sit at edge midpoints; key each by the edge it lies on so
    # neighboring cells share endpoints exactly. (:H, i, j) is the midpoint of
    # the horizontal edge from grid point (i,j) to (i,j+1); (:V, i, j) likewise.
    pt(key) = key[1] === :H ? (key[3] + 0.5, Float64(key[2])) : (Float64(key[3]), key[2] + 0.5)

    segs = Tuple{NTuple{3, Any}, NTuple{3, Any}}[]
    for i in 1:(g - 1), j in 1:(g - 1)
        tl, tr = mask[i, j], mask[i, j + 1]
        bl, br = mask[i + 1, j], mask[i + 1, j + 1]
        top = (:H, i, j)
        bottom = (:H, i + 1, j)
        left = (:V, i, j)
        right = (:V, i, j + 1)
        ct = (tl != tr) + (tr != br) + (br != bl) + (bl != tl)
        if ct == 2
            es = NTuple{3, Any}[]
            tl != tr && push!(es, top)
            tr != br && push!(es, right)
            br != bl && push!(es, bottom)
            bl != tl && push!(es, left)
            push!(segs, (es[1], es[2]))
        elseif ct == 4
            # Saddle: isolate the two inside corners.
            if tl
                push!(segs, (top, left))
                push!(segs, (right, bottom))
            else
                push!(segs, (top, right))
                push!(segs, (bottom, left))
            end
        end
    end

    neigh = Dict{NTuple{3, Any}, Vector{NTuple{3, Any}}}()
    for (a, b) in segs
        push!(get!(neigh, a, NTuple{3, Any}[]), b)
        push!(get!(neigh, b, NTuple{3, Any}[]), a)
    end
    canon(a, b) = isless(a, b) ? (a, b) : (b, a)
    used = Set{Tuple{NTuple{3, Any}, NTuple{3, Any}}}()
    loops = Vector{Tuple{Float64, Float64}}[]
    for (a0, b0) in segs
        canon(a0, b0) in used && continue
        push!(used, canon(a0, b0))
        loop = [pt(a0)]
        prev, cur = a0, b0
        while cur != a0
            push!(loop, pt(cur))
            nxt = nothing
            for cand in neigh[cur]
                cand == prev && continue
                if !(canon(cur, cand) in used)
                    push!(used, canon(cur, cand))
                    nxt = cand
                    break
                end
            end
            nxt === nothing && break
            prev, cur = cur, nxt
        end
        length(loop) >= 3 && push!(loops, loop)
    end
    return loops, g
end

function render_svg(path, c, cx, cy, extent, maxiter, g, smooth)
    loops, g = contour_loops(set_mask(c, cx, cy, extent, maxiter, g, smooth))
    pad = g * 0.04
    vb = g + 2pad
    f(x) = round(x + pad - 1; digits = 2)
    d = IOBuffer()
    for loop in loops
        print(d, "M", f(loop[1][1]), ",", f(loop[1][2]))
        for k in 2:length(loop)
            print(d, "L", f(loop[k][1]), ",", f(loop[k][2]))
        end
        print(d, "Z")
    end
    stops = join(
        [
            "<stop offset=\"$(round(Int, p * 100))%\" stop-color=\"#$(string(col[1], base = 16, pad = 2))$(string(col[2], base = 16, pad = 2))$(string(col[3], base = 16, pad = 2))\"/>"
                for (p, col) in GRADIENT
        ], ""
    )
    return open(path, "w") do io
        print(
            io, """
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 $(round(vb, digits = 2)) $(round(vb, digits = 2))">
              <defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="1">$stops</linearGradient></defs>
              <path fill="url(#g)" fill-rule="evenodd" d="$(String(take!(d)))"/>
            </svg>
            """
        )
    end
end

# --- main -------------------------------------------------------------------

function main(args)
    mode = getarg(args, "mode", "julia")
    newton = mode == "newton"
    preset = getarg(args, "preset", "rabbit")
    c = haskey(PRESETS, preset) ? PRESETS[preset] : PRESETS["rabbit"]
    cs = getarg(args, "c", nothing)
    cs !== nothing && (c = parsecomplex(cs))
    center = parsecomplex(getarg(args, "center", "0,0"))
    extent = parse(Float64, getarg(args, "extent", newton ? "1.6" : "1.45"))
    maxiter = parse(Int, getarg(args, "maxiter", newton ? "60" : "250"))
    png = parse(Int, getarg(args, "png", "720"))
    ss = parse(Int, getarg(args, "ss", "3"))
    grid = parse(Int, getarg(args, "grid", "640"))
    smooth = parse(Float64, getarg(args, "smooth", "0.0"))
    whiten = parse(Float64, getarg(args, "whiten", "1.0"))
    border = parse(Float64, getarg(args, "border", newton ? "0.015" : "0.0"))
    out = getarg(args, "out", newton ? "assets/fatou-newton" : "assets/fatou-logo")

    mkpath(dirname(out) == "" ? "." : dirname(out))
    cx, cy = real(center), imag(center)

    if newton
        println("Fatou logo — Newton fractal for z³ - 1")
        println("  view: center=($cx, $cy) extent=$extent maxiter=$maxiter threads=$(Threads.nthreads())")
        print("  rendering PNG ($(png)×$(png), ss=$ss) … ")
        t = @elapsed render_newton_png("$out.png", cx, cy, extent, maxiter, png, ss, whiten, border)
        println("$(round(t, digits = 2))s → $out.png")
        return  # the basin boundary is infinitely intricate — raster only
    end

    println("Fatou logo — c = $(real(c)) + $(imag(c))i, preset=$preset")
    println("  view: center=($cx, $cy) extent=$extent maxiter=$maxiter threads=$(Threads.nthreads())")

    print("  rendering PNG ($(png)×$(png), ss=$ss) … ")
    t = @elapsed render_png("$out.png", c, cx, cy, extent, maxiter, png, ss, smooth)
    println("$(round(t, digits = 2))s → $out.png")

    print("  rendering SVG (grid=$grid) … ")
    t = @elapsed render_svg("$out.svg", c, cx, cy, extent, maxiter, grid, smooth)
    return println("$(round(t, digits = 2))s → $out.svg")
end

main(ARGS)
