#import <Foundation/Foundation.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

/// C-compatible config struct passed to Rust during bootstrap.
/// Layout must match `VixenFsConfig` in `vx-fskit/src/macos/extension_runtime/mod.rs`.
typedef struct {
    const char * _Nonnull bundle_version;
    const char * _Nonnull bundle_identifier;
    const char * _Nonnull app_group_path;
    const char * _Nonnull socket_path;
    bool is_debug;
} VixenFsConfig;

void * _Nullable VixenRustCreateFileSystem(const VixenFsConfig * _Nonnull config);

#ifdef __cplusplus
}
#endif
