function ⊑ end
function ⊇ end
function ∘ end
function + end
function ≤ end
macro + end
typename(typeof(function + end)).constprop_heuristic = Core.SAMETYPE_HEURISTIC
const ⊆ = issubset
function ⊑(𝕃::AbstractLattice, a, b) end
function +(x) end
function -(x, y)
    x - y
end
