Vendored Perfetto protobuf schema for trace export.

Source:
- crate: `tracing-perfetto-sdk-schema` `0.13.1`
- file: `protos/perfetto_trace.proto`

Why this is vendored:
- Vixen generates committed Rust bindings via `cargo xtask perfetto-proto`
- normal builds do not need `build.rs`
- normal builds do not need `protoc`

Do not edit `perfetto_trace.proto` or generated bindings by hand unless you are
intentionally updating the vendored Perfetto schema.
