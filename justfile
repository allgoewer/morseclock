alias c := check

_list:
    @just -l

# Check the source-code for errors
check:
    cargo clippy
