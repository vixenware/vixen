# Vixen development Justfile
# Shared VX_HOME for manual debugging

# List available tasks
list:
    just --list

build-mac-app:
    just apps/mac/build
