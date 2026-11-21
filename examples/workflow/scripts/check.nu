let result = (run-external $env.PROD_EXECUTABLE | complete)
print --no-newline $result.stdout
print --stderr --no-newline $result.stderr

if $result.exit_code not-in [0 1] {
    error make {msg: "page-table test did not complete"}
}

let failures = ($result.stdout | lines | where {|line| $line | str starts-with "FAIL "})
let passed = ($result.exit_code == 0)
if (not $passed) and ($failures | is-empty) {
    error make {msg: "page-table test exited without a diagnostic"}
}
let summary = if $passed { "All page-table checks passed" } else { $failures | str join ", " }
{passed: $passed, summary: $summary} | to json | save --force $env.PROD_OUTPUT_JSON
