# FSKit Hermetic Summit Receipt

Date: 2026-07-08

Branch: `taxon-hermetic-summit-receipt`

Fold base: vixen `main` at `88bc7d003680d6c8a564281bcefd5f7fa4052719`

FSKit provider-lifetime repair: `d8dc1b8a1de33fc1d176ae05c0a89dd514b7cb91`

## Live Mount

The live mounted FSKit extension reported the patched build before the proof
run:

```json
{"connected":true,"bundle_version":"0.6.3","bundle_identifier":"eu.bearcove.vixenfs","ever_connected":true,"app_group_path":"/Users/amos/Library/Group Containers/B2N6FSRTPV.vixenfs","socket_path":"/Users/amos/Library/Group Containers/B2N6FSRTPV.vixenfs/vixenfs.sock","extension_pid":55302,"live_prefixes":["app"]}
```

The proof used the existing mount at `/Volumes/Vixen/vfs`.

## Command

```sh
cargo nextest run -p vx-exec -E 'test(live_fskit_vfs_executor_uses_tracked_observations_as_read_set)' --run-ignored ignored-only --no-capture
```

## Result

```text
Nextest run ID 4ab16211-de4e-4179-b235-4de8e78f81f4 with nextest profile: default
Starting 1 test across 3 binaries (10 tests skipped)
START vx-exec::sandboxed_executor live_fskit_vfs_executor_uses_tracked_observations_as_read_set

2026-07-08T05:50:41.914836Z  INFO vx_vfsd: canary lookup hit parent_id=1 name=.vfs-smoke-ff3c6ea1dd88a1bf item_id=18446744073709355009 item_type=File mode=420 size=32
2026-07-08T05:50:41.915603Z  INFO vx_vfsd: canary get_attributes hit item_id=18446744073709355009 result_item_id=18446744073709355009 item_type=File mode=420 size=32
2026-07-08T05:50:41.917350Z  INFO vx_vfsd: canary get_attributes hit item_id=18446744073709355009 result_item_id=18446744073709355009 item_type=File mode=420 size=32
2026-07-08T05:50:41.930961Z  INFO vx_vfsd::service: mounting tree into VFS path=/exec-41716-1783489841921242000/m/in tree_hash=71dc4eb4f086f4d1d1d0e27452069635d16fbc073b3c6d74c86acd212a473a75

test live_fskit_vfs_executor_uses_tracked_observations_as_read_set ... ok
Summary: 1 test run: 1 passed, 10 skipped
```

## Observed Read-Set

The runner's read-set authority was `vx-vfsd` `tracked_observations(prefix_id,
roots)`, mapped through `FskitObservationFeed` into `ExecReadSet`. The process
read through the mounted virtual prefix, not through copied staging.

```text
ExecReadSet {
  entries: {
    ExecPath("/m/in/._message.txt"): LookupMiss {
      parent_path: ExecPath("/m/in"),
      directory_node: NodeHash(Blake3Hash([113, 220, 78, 180, 240, 134, 244, 209, 209, 208, 226, 116, 82, 6, 150, 53, 209, 111, 188, 7, 59, 60, 109, 116, 200, 106, 205, 33, 42, 71, 58, 117]))
    },
    ExecPath("/m/in/message.txt"): File {
      content_hash: ContentHash(Blake3Hash([74, 48, 164, 207, 208, 127, 155, 141, 14, 226, 70, 37, 33, 9, 229, 60, 120, 85, 8, 249, 165, 70, 245, 221, 233, 27, 92, 126, 185, 49, 19, 29])),
      blob_node: None,
      size: ByteLen(22)
    }
  }
}
```

The declared output was harvested successfully:

```text
/m/out/live.txt = "hello from live fskit\n"
```

This is the live receipt for the two FSKit primitives used by the hermetic
runner path: per-exec virtual prefixes and exact tracked observations. No
copy-staging read-set synthesis participated in this proof.
