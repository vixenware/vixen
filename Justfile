# Vixen development Justfile

# Shared VX_HOME for manual debugging
debug_home := "/tmp/vx-debug"

# Run vx-cass with full logging
cass:
    mkdir -p {{debug_home}}
    RUST_LOG=trace VX_HOME={{debug_home}} cargo run --bin vx-cass

# Run vx-rhea with full logging (start cass first!)
rhea:
    mkdir -p {{debug_home}}
    RUST_LOG=trace VX_HOME={{debug_home}} cargo run --bin vx-rhea

# Run vx-aether with full logging (start cass and rhea first!)
aether:
    mkdir -p {{debug_home}}
    RUST_LOG=trace VX_HOME={{debug_home}} cargo run --bin vx-aether

# Run a test against the manually-started services
test name:
    VX_TEST_EXTERNAL_DAEMON=1 VX_TEST_VX_HOME={{debug_home}} cargo nextest run {{name}} --nocapture

# Clean up debug state (sockets, mount, etc.)
debug-clean:
    -umount {{debug_home}}/fskitty/rhea 2>/dev/null
    rm -rf {{debug_home}}
