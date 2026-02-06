# Vixen development Justfile
# Shared VX_HOME for manual debugging

debug_home := "/tmp/vx-debug"
rust_log := "trace"

# List available tasks
list:
    just --list

# Run vx-cass with full logging
cass *args:
    mkdir -p {{ debug_home }}
    RUST_LOG={{ rust_log }} VX_HOME={{ debug_home }} cargo run --bin vx-cass {{ args }}

# Run vx-rhea with full logging (start cass first!)
rhea *args:
    mkdir -p {{ debug_home }}
    RUST_LOG={{ rust_log }} VX_HOME={{ debug_home }} cargo run --bin vx-rhea {{ args }}

# Run vx-aether with full logging (start cass and rhea first!)
aether *args:
    mkdir -p {{ debug_home }}
    RUST_LOG={{ rust_log }} VX_HOME={{ debug_home }} cargo run --bin vx-aether {{ args }}

# Run a test against the manually-started services
test name:
    VX_TEST_EXTERNAL_DAEMON=1 VX_TEST_VX_HOME={{ debug_home }} cargo nextest run {{ name }} --nocapture

test-all *args:
    VX_TEST_EXTERNAL_DAEMON=1 VX_TEST_VX_HOME={{ debug_home }} cargo nextest run {{ args }}

# Generate dSYMs for all vx debug binaries
dsym-debug:
    @found=0; \
    for bin in target/debug/vx-*; do \
        [ -e "$bin" ] || continue; \
        [ -f "$bin" ] || continue; \
        [ -x "$bin" ] || continue; \
        case "$bin" in \
            *.dSYM|*.d|*.rlib|*.rmeta) continue ;; \
        esac; \
        found=1; \
        echo "dsymutil $bin"; \
        dsymutil "$bin"; \
    done; \
    if [ "$found" -eq 0 ]; then \
        echo "No vx debug binaries found under target/debug (build first)." >&2; \
        exit 1; \
    fi

# Clean up debug state (sockets, etc.)
debug-clean:
    rm -rf {{ debug_home }}
