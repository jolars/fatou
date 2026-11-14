#!/usr/bin/env julia
# Generate the stepped basin triad and its published assets. Favicons are managed separately.
include("generate-logo.jl")

# Four-connected components keep neighboring basins separate at their boundary.
function components(labels)
    seen = falses(size(labels))
    groups = Tuple{UInt8, Vector{CartesianIndex{2}}}[]
    steps = CartesianIndex.(((-1, 0), (1, 0), (0, -1), (0, 1)))
    for p in CartesianIndices(labels)
        (seen[p] || labels[p] == 0) && continue
        root = labels[p]
        queue = [p]
        seen[p] = true
        head = 1
        while head <= length(queue)
            q = queue[head]
            head += 1
            for step in steps
                r = q + step
                if checkbounds(Bool, labels, r) && !seen[r] && labels[r] == root
                    seen[r] = true
                    push!(queue, r)
                end
            end
        end
        push!(groups, (root, queue))
    end
    return groups
end

# Only keep whole islands inside the composition. The three unbounded basins
# and any island crossing the radius are discarded, never clipped into a frame.
function newton_layers(n, extent, radius)
    roots = (complex(1.0, 0.0), cispi(2 / 3), cispi(4 / 3))
    labels = zeros(UInt8, n, n)
    opacity = zeros(Float64, n, n)
    iterations = zeros(UInt16, n, n)
    Threads.@threads for j in 1:n
        for i in 1:n
            z = to_complex(i, j, n, n, 0.0, 0.0, extent)
            root, k = newton_basin(z, roots, 60, 1.0e-12)
            iterations[i, j] = k
            # Replace the old white convergence tint with transparency. RGB
            # stays the basin color, avoiding a white fringe on dark surfaces.
            opacity[i, j] = 1 - clamp(20 * (k / 60)^2, 0.0, 1.0)
            # Invisible boundary samples must not connect otherwise separate
            # islands through subpixel filaments in the sampled basin labels.
            labels[i, j] = opacity[i, j] > 0 ? root : 0
        end
    end
    masks = [falses(n, n) for _ in 1:3]
    largest = zeros(Int, 3)
    for (root, group) in components(labels)
        inside = all(group) do p
            i, j = Tuple(p)
            return abs(to_complex(i, j, n, n, 0.0, 0.0, extent)) < radius
        end
        inside || continue
        length(group) <= largest[root] && continue
        largest[root] = length(group)
        fill!(masks[root], false)
        masks[root][group] .= true
    end
    masks = [simplify_mask(mask, 0.006) for mask in masks]
    fill!(opacity, 1.0)
    return collect(zip(masks, NEWTON_COLORS)), opacity, iterations
end

tint(color, amount) = ntuple(k -> round(UInt8, color[k] + amount * (255 - color[k])), 3)

function convergence_layers(layers, iterations, thresholds; edge_tint)
    result = Tuple{BitMatrix, NTuple{3, UInt8}}[]
    for (mask, color) in layers
        lower = -1
        for (band, upper) in enumerate(thresholds)
            region = BitMatrix(mask .& (lower .< iterations) .& (iterations .<= upper))
            lower = upper
            any(region) || continue
            shade = edge_tint * (band - 1) / (length(thresholds) - 1)
            push!(result, (region, tint(color, shade)))
        end
    end
    return result
end

# Marching squares emits many points along the same straight segment. Keep
# only turns and encode relative integer coordinates on a doubled grid. This
# preserves the contour exactly while avoiding a large coordinate list.
function write_svg_loop(io, loop)
    points = Tuple{Int, Int}[]
    for k in eachindex(loop)
        prev = loop[mod1(k - 1, length(loop))]
        cur = loop[k]
        next = loop[mod1(k + 1, length(loop))]
        dx1, dy1 = cur[1] - prev[1], cur[2] - prev[2]
        dx2, dy2 = next[1] - cur[1], next[2] - cur[2]
        if dx1 * dy2 == dy1 * dx2 && dx1 * dx2 + dy1 * dy2 > 0
            continue
        end
        push!(points, (round(Int, 2cur[1] - 1), round(Int, 2cur[2] - 1)))
    end
    isempty(points) && return
    print(io, "M", points[1][1], ",", points[1][2])
    for k in 2:length(points)
        dx = points[k][1] - points[k - 1][1]
        dy = points[k][2] - points[k - 1][2]
        if dy == 0
            print(io, "h", dx)
        elseif dx == 0
            print(io, "v", dy)
        else
            print(io, "l", dx, ",", dy)
        end
    end
    print(io, "Z")
end

function write_logo(prefix, layers, opacity, ss; vector = true)
    n = size(opacity, 1)
    pixels = div(n, ss)
    img = zeros(UInt8, pixels, pixels, 4)
    for bi in 1:pixels, bj in 1:pixels
        rgb = zeros(3)
        alpha = 0.0
        for di in 1:ss, dj in 1:ss
            i = (bi - 1) * ss + di
            j = (bj - 1) * ss + dj
            for (mask, color) in layers
                mask[i, j] || continue
                a = opacity[i, j]
                for channel in 1:3
                    rgb[channel] += a * color[channel]
                end
                alpha += a
            end
        end
        if alpha > 0
            img[bi, bj, 1:3] .= round.(UInt8, rgb ./ alpha)
            img[bi, bj, 4] = round(UInt8, 255alpha / ss^2)
        end
    end
    visible = findall(!iszero, @view img[:, :, 4])
    imin, imax = extrema(p[1] for p in visible)
    jmin, jmax = extrema(p[2] for p in visible)
    pad = max(1, round(Int, 0.02 * max(imax - imin + 1, jmax - jmin + 1)))
    top = max(1, imin - pad)
    left = max(1, jmin - pad)
    bottom = min(pixels, imax + pad)
    right = min(pixels, jmax + pad)
    img = img[top:bottom, left:right, :]
    write_png("$prefix.png", img)
    run(`magick $prefix.png -strip -define png:compression-level=9 $prefix.png`)
    vector || return
    open("$prefix.svg", "w") do io
        # The same crop in supersampled coordinates keeps PNG and SVG framing
        # aligned without rescaling the artwork or changing its contour paths.
        x, y = 2(left - 1) * ss, 2(top - 1) * ss
        w, h = 2size(img, 2) * ss, 2size(img, 1) * ss
        println(io, "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"$x $y $w $h\">")
        println(io, "  <title>Fatou logo</title>")
        for (mask, color) in layers
            loops, _ = contour_loops(mask)
            hex = join(string(c; base = 16, pad = 2) for c in color)
            print(io, "  <path fill=\"#$hex\" fill-rule=\"evenodd\" d=\"")
            for loop in loops
                write_svg_loop(io, loop)
            end
            println(io, "\"/>")
        end
        println(io, "</svg>")
    end
end

function brand_main()
    cd(joinpath(@__DIR__, "..")) do
        ss = 3
        layers, opacity, iterations = newton_layers(512ss, 1.0, 0.85)
        thresholds = (4, 5, 6, 7, 8, 9, 10, 12, typemax(UInt16))
        for (suffix, edge_tint) in (("", 0.58), ("-dark", 0.70))
            bands = convergence_layers(layers, iterations, thresholds; edge_tint)
            prefix = "assets/logo$suffix"
            write_logo(prefix, bands, opacity, ss)
            cp("$prefix.png", "docs/src/public/images/logo$suffix.png"; force = true)
        end
        # Render the large PNG directly from a denser basin grid. Adjacent
        # contour bands share pixel coverage, avoiding seams from SVG rendering.
        large_layers, large_opacity, large_iterations = newton_layers(3072, 1.0, 0.85)
        for (suffix, edge_tint) in (("", 0.58), ("-dark", 0.70))
            bands = convergence_layers(large_layers, large_iterations, thresholds; edge_tint)
            prefix = "docs/src/public/images/logo-hero$suffix"
            write_logo(prefix, bands, large_opacity, 1; vector = false)
            run(`magick $prefix.png -resize 1440x -strip -define png:compression-level=9 $prefix.png`)
            run(`magick $prefix.png -resize 128x128 -strip -define png:compression-level=9
                $("docs/src/public/images/logo-small$suffix.png")`)
        end
        # The marketplace icon needs a square canvas; center the cropped mark
        # with a small inset rather than reintroducing the original margins.
        run(`magick docs/src/public/images/logo-hero.png -resize 484x484 -gravity center -background none
            -extent 512x512 -strip -define png:compression-level=9 editors/code/icon.png`)
        println("Updated the basin triad logo, large and small documentation images, and extension icon.")
    end
end

if abspath(PROGRAM_FILE) == @__FILE__
    brand_main()
end
