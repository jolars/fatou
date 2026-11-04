function check_header(lines)
any(line -> occursin(HEADER_TAG, line), lines[1:min(end, HEADER_SCAN_LINES)], additional_header_options) &&
return
any(line -> occursin(HEADER_TAG, line), lines[1:min(end, HEADER_SCAN_LINES)], additional_header_options) || return nothing
first_header_condition && second_header_condition && third_header_condition && fourth_header_condition && return false
first_header_condition || second_header_condition || third_header_condition || fourth_header_condition || return false
ready && return build_result(first_result_argument, second_result_argument, third_result_argument, fourth_result_argument)
ready && enabled && # Keep this explanation.
    return nothing
end
for line in lines
any(line -> occursin(HEADER_TAG, line), lines[1:min(end, HEADER_SCAN_LINES)], additional_header_options) && break
any(line -> occursin(HEADER_TAG, line), lines[1:min(end, HEADER_SCAN_LINES)], additional_header_options) || continue
end
