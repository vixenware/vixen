# Investigation Notes: FSKit Mount & Sandbox Issues

## Noise to IGNORE (these don't matter):

**AMFI errors - always present, system still works:**
```
AMFI: '/private/tmp/vixen-tests/debug/vx-store' has no CMS blob?
AMFI: '/private/tmp/vixen-tests/debug/vx-store': Unrecoverable CT signature issue, bailing out.
```

**Entitlement messages - don't matter:**
```
fskitd: Incomming connection, entitled 0
fskitd: Hello FSClient! entitlement no
```

**Container manager rejections - don't matter:**
```
containermanagerd: [eu.bearcove.vixenfs] requesting [group.eu.bearcove.vixen]: REJECTED. Requestor's signature does not allow it to access a TCC-protected group container.
```

## Facts:

1. ✅ **FSKit mount WORKS at** `$VX_HOME/vixen/runner` (e.g., `/tmp/.vx-test/nextest/home/vixen/runner`)
2. ❌ **FSKit mount FAILS at** `/Volumes/*` with "invalid file system" error
3. ❌ **Sandbox blocks execution** from mounted VFS:
   ```
   System Policy: rustc(69237) deny(1) file-read-data /private/tmp/.vx-test/nextest/home/vixen/runner/tc-.../bin/rustc
   ```

## Off-Limits (DO NOT SUGGEST):

- ❌ Moving toolchain out of VFS
- ❌ Mounting in `/Volumes/`
- ❌ Focusing on AMFI/entitlement noise

## Investigation TODO:

1. **Test smaller binaries**: Try executing simple binaries (`/bin/echo`, small Rust binary) from VFS
2. **Test file operations**: Read files, list directories - what works vs what fails?
3. **Try ad hoc signing**: `codesign -s - <binary>` on binaries in VFS
4. **Examine sandbox profile**: What's actually blocking execution?
5. **Compare working case**: Execution from `/tmp` directly vs from VFS mount at `/tmp/.../vixen/runner`

## Current Problem

**Sandbox denies file-read-data on executables in the VFS mount**

The mount succeeds. Files exist. But when trying to execute binaries from the mounted VFS, macOS System Policy blocks file-read-data operations, preventing execution.
