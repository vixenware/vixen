# Strict macOS seatbelt profile

This note is the audit trail for the macOS substrate sandbox. The rule is
stricter than "no security escape": every extra read path is also a
determinism hole, because undeclared host state can poison a content-addressed
cache key.

Audited Apple artifact:

| Field | Value |
| --- | --- |
| Path | `/System/Library/Sandbox/Profiles/system.sb` |
| Line count | 311 |
| SHA-256 | `180a0356611b5de7a7db84b800ed47a1d2f15ec884d0fb960445366ededac7c8` |
| Pin | `crates/seatbelt-exec/system-sb.sha256` |
| Failing test | `seatbelt-exec::seatbelt audited_system_sb_hash_matches_this_host` |

If Apple changes that file, the test fails loudly. The claim is tied to the
audited artifact, not to the filename.

## Recovered History

`f4effa0a` had the right direction: `(deny default)`, import `system.sb`,
parameterized VFS/write roots, explicit file read/write allowances, explicit
toolchain `process-exec`, and `deny network*`.

`d3c2b328` regressed by switching to `(allow default)` and then denying only
network and writes. That made startup easy, but it gave up read confinement.
In cache terms, the process could still read undeclared state, so the sandbox
was no longer the enforcement arm of hermeticity.

The new crate, `crates/seatbelt-exec`, returns to deny-default and keeps the
escaping/parameter discipline from the recovered work.

## Final Profile Shape

The generator is `SeatbeltPlan::compile()`:

1. `(deny default)`
2. `(import "system.sb")`
3. claw back imported file grants:
   `/tmp`, `/var`, `/private/tmp`, `/private/var`, `/Users`, `/Library`,
   `/usr/share`, `/private/etc`
4. `(deny file-write*)`
5. `(deny network*)`
6. allow `process-fork`
7. allow metadata/test-existence only on ancestors of declared roots
8. allow `process-exec` only under declared toolchain roots
9. allow file reads only under declared read roots, declared write roots, and
   declared toolchain roots
10. allow executable maps only under declared toolchain roots
11. allow writes only under declared write roots

All caller-provided roots are canonicalized before profile generation. This is
load-bearing on macOS because `/tmp` resolves to `/private/tmp` before the
seatbelt check.

## Clawback List

| Imported grant | Clawback | Why |
| --- | --- | --- |
| `/tmp`, `/private/tmp` metadata/read grants | deny, then re-open only declared roots | Temp directories are ambient mutable state. Tests prove compiles work when only the source/write roots under `/private/tmp` are re-opened. |
| `/var`, `/private/var` | deny | `/usr/bin/cc` tried to read `/var/select/developer_dir`; that wrapper is intentionally excluded. Direct toolchain binaries are required. |
| `/Users` | deny, then re-open explicit toolchain roots such as rustup sysroots | `$HOME` is undeclared host state. The rustc test uses the direct sysroot binary and only re-opens that sysroot. |
| `/Library` | deny, then re-open explicit toolchain roots if a caller uses CommandLineTools there | The tested Xcode clang compile does not need ambient `/Library`. |
| `/usr/share` | deny | Not needed by the clang or rustc proof compiles. |
| `/private/etc` | deny | Not needed by the clang or rustc proof compiles. |
| all writes | deny, then re-open declared write roots | Write-set confinement is the executable cache contract. |
| all network | deny | Network is undeclared input/output. |

## `system.sb` Allow Audit

Categories:

- `bootstrap-essential`: kept because process startup, dyld/libsystem,
  syscall/mach plumbing, or libc runtime behavior needs it under the imported
  profile.
- `file-access grant`: scrutinized as a determinism hole. The final profile
  either claws it back or re-opens a narrower declared root after the clawback.
- `apple-internal/inapplicable`: not active on normal hosts, guarded by an
  Apple-only filter, or inside a helper definition this profile never calls.

| Lines | Allow form | Category | Decision |
| --- | --- | --- | --- |
| 15 | `mach-bootstrap` | bootstrap-essential | Keep. Process startup plumbing. |
| 16 | `syscall*` | bootstrap-essential | Keep through import-path guard. |
| 19 | `mach-register` local prefix | bootstrap-essential | Keep. Per-pid service registration plumbing. |
| 23 | broad XPC `mach-lookup` compatibility | bootstrap-essential | Keep for libsystem/XPC compatibility inherited from Apple profile. |
| 26-29 | `system-automount` for restricted platform binaries | apple-internal/inapplicable | Inapplicable to non-platform compiler children. |
| 32-40 | standard system file reads | file-access grant | Partially kept for `/System` and `/usr/lib`; `/private/var`, `/Library`, and `/usr/share` are clawed back. |
| 43-55 | executable maps for system frameworks/dylibs | bootstrap-essential | Keep. Needed for dyld/libsystem mappings. |
| 59-68 | AppleInternal framework/lib reads | apple-internal/inapplicable | Guarded by AppleInternal filter. |
| 69-70 | AppleInternal malloc sysctl write | apple-internal/inapplicable | Guarded by AppleInternal filter. |
| 73 | `SYS_csrctl` check | bootstrap-essential | Keep. Apple documents this as variant/security-policy check plumbing. |
| 76 | `vnguard` mac syscall | bootstrap-essential | Keep. Guarded vnode plumbing. |
| 79-82 | Sandbox mac syscall 67 | bootstrap-essential | Keep. Container expectation check. |
| 85-89 | symlink metadata for `/etc`, `/tmp`, `/var`, localtime | file-access grant | `/tmp` and `/var` are clawed back; `/etc` metadata remains only as system symlink resolution. |
| 92-93 | firmlink ancestor metadata | bootstrap-essential | Keep. Metadata only for system firmlink traversal. |
| 97-98 | root directory read/existence | bootstrap-essential | Keep. Required for inherited cwd/root traversal. |
| 101 | `FSIOC_CAS_BSDFLAGS` fsctl | bootstrap-essential | Keep. Copyfile/chflags compatibility. |
| 104-111 | special file and `/private/etc/*` reads | file-access grant | Device entropy reads kept; `/private/etc` is clawed back after import. |
| 113-115 | `/dev/null`, `/dev/zero` read/write | bootstrap-essential | Keep. Standard process plumbing. |
| 119-120 | `/dev/fd` read/write | bootstrap-essential | Keep. POSIX fd behavior. |
| 122-123 | `/dev/dtracehelper` read/write/ioctl | bootstrap-essential | Keep by import; not observed as a data input in proof compiles. |
| 126-127 | eligibility plist read | file-access grant | Clawed back by `/private/var` deny. |
| 131-132 | posix spawn filtering rules | apple-internal/inapplicable | Guarded by AppleInternal filter. |
| 135-137 | core dump creation | file-access grant | Superseded by later `deny file-write*`; not re-opened. |
| 141-144 | AppleInternal logging reads/maps | apple-internal/inapplicable | Guarded by AppleInternal filter. |
| 147-148 | Rosetta runtime metadata | bootstrap-essential | Keep. Metadata only, harmless on arm64 proof lane. |
| 151-152 | syslog socket outbound | bootstrap-essential | Keep as Apple logging IPC; `deny network*` later blocks general networking. |
| 154-156 | notification/cfprefs shm reads | bootstrap-essential | Keep. libc/system preferences plumbing. |
| 158-179 | standard system agent `mach-lookup` | bootstrap-essential | Keep. System service lookup plumbing, not file input authority. |
| 182 | AppleInternal objc trace lookup | apple-internal/inapplicable | Guarded by AppleInternal filter. |
| 185 | platform osanalytics lookup | apple-internal/inapplicable | Platform-binary-only filter. |
| 187-188 | debug syscall reject | bootstrap-essential | Keep. Kernel debug rejection path. |
| 191 | `sysctl-read` | bootstrap-essential | Keep. Runtime CPU/OS probing; accepted bootstrap surface. |
| 192-194 | narrow `sysctl-write` | bootstrap-essential | Keep. Apple marks workqueue/CPU-grade runtime knobs. |
| 197-199 | MIG vm reallocate | bootstrap-essential | Keep. libmalloc requirement. |
| 204-207 | graphics preferences | apple-internal/inapplicable | Inside `system-graphics`, which this profile never calls. |
| 209-221 | graphics mach lookups | apple-internal/inapplicable | Inside unused `system-graphics`. |
| 222-223 | CVMS database reads | apple-internal/inapplicable | Inside unused `system-graphics`; would be a file grant if called. |
| 225-256 | graphics IOKit service/user-client opens | apple-internal/inapplicable | Inside unused `system-graphics`. |
| 258-259 | GPU bundle reads | apple-internal/inapplicable | Inside unused `system-graphics`; would be a file grant if called. |
| 261-270 | display IOKit property sets | apple-internal/inapplicable | Inside unused `system-graphics`. |
| 274-277 | network preference/db reads | apple-internal/inapplicable | Inside `system-network`, which this profile never calls. |
| 278-287 | network service mach lookups | apple-internal/inapplicable | Inside unused `system-network`. |
| 288-290 | network outbound controls | apple-internal/inapplicable | Inside unused `system-network`; also blocked by explicit `deny network*`. |
| 291-294 | route/control sockets | apple-internal/inapplicable | Inside unused `system-network`. |
| 296-300 | CFNetwork/AppSSO mach lookups | apple-internal/inapplicable | Inside unused `system-network`. |
| 301-305 | AppSSO shm/preferences | apple-internal/inapplicable | Inside unused `system-network`. |
| 310-311 | OOPJIT extension file access | apple-internal/inapplicable | Inside `oopjit-runner`, which this profile never calls. |

## Proof Matrix

All proof tests live in `crates/seatbelt-exec/tests/seatbelt.rs` and invoke
real `/usr/bin/sandbox-exec`.

| Test | Expected result | What it proves |
| --- | --- | --- |
| `audited_system_sb_hash_matches_this_host` | pass | Apple `system.sb` still matches the audited 311-line artifact. |
| `clang_compile_from_declared_mount_succeeds` | pass | A real direct Xcode clang compile succeeds with only declared source, write, and toolchain roots. |
| `rustc_compile_from_declared_mount_succeeds` | pass | A real direct rustc compile succeeds with only declared source, write, and sysroot roots; the rustup shim is not used inside the sandbox. |
| `undeclared_reads_network_and_writes_are_denied` | pass | Reads from `/tmp`, `/var`, and `$HOME` fail; network connect fails with `Operation not permitted`; writes outside `WRITE_ROOT` do not land. |

Manual trace that shaped the policy:

- `/usr/bin/cc` failed by trying to read `/var/select/developer_dir`.
  Decision: do not allow it; use a concrete toolchain binary discovered before
  sandbox entry.
- Direct Xcode clang succeeded without SDK, `/Library`, `/usr/share`, or
  `/private/etc` allowances.
- Direct rustc succeeded with only the canonical sysroot re-opened under
  `/Users`.

The profile is trace-driven-minimal for the current proof compiles. Future
tools must add declared toolchain/read/write roots, not ambient host paths.
