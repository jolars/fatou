@inline function Base.getindex(df::DataFrame, row_inds::AbstractVector{T}, ::Colon) where T
    return new_df
end
