# Capabilities: the ambient residue (Amos, 2026-07-08)

The load-bearing distinction (supersedes the "capabilities are dependencies" sketch's
emphasis, which survives only as the runner-matching layer):

- **Materializable toolchains (rustc-class)**: download/extract into the local CAS, mount
  via VFS prefix — or mount from a REMOTE store without downloading at all. Identity is
  a-priori (we hash what we mount); immutability by construction. These are NOT deeply
  capabilities — they are ordinary content-addressed inputs, and the set grows over time.
- **Ambient toolchains (Xcode/MSVC-class)**: technically and LEGALLY impossible to
  materialize (redistribution forbidden; entangled with the OS). These are the real
  capabilities. Contract: the DAEMON identifies, fingerprints, and FS-WATCHES
  (FSEvents/inotify) the entire toolchain tree, and POISONS ANY IN-FLIGHT BUILD if the
  toolchain is modified in place. Identity is a-posteriori and continuously re-verified
  for the duration of every build that uses it.

Consequences:
- Two trust postures: mount-what-you-hashed vs fingerprint-what-you-must-trust-and-watch.
  The poison-on-mutation contract is what keeps receipts honest over toolchains we can
  only observe.
- The capability system is a deliberately SHRINKING boundary: things migrate out of it
  and into the store as materialization becomes possible. rustc already crossed; host cc
  (= Xcode clang on macOS) has not and may never.
- The demo's C-toolchain handling should follow the fingerprint discipline now and gain
  daemon-watch when composed with the runner (the capability advertisement carries the
  fingerprint; the watch is the daemon's ambient service).
- Prior design work on this exists in git history (deleted-daemon era) — recovery scout
  dispatched 2026-07-08 (documents/design only, never code resurrection).
