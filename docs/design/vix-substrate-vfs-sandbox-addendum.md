# Addendum: git-history corrections to the sandbox recon

The recon reported macOS enforcement as "aspirational doc comments
only." History says otherwise — BOTH halves exist:

## The deny-default profile (the 12h stretch): f4effa0a (#165, Feb 10)
"Sandbox compiler subprocesses with macOS seatbelt." A FULL
deny-default SBPL profile, parameterized, all four confinement axes:

    (version 1)
    (debug deny)
    (deny default)
    (import "system.sb")          ; THE unlock — Apple's own base
                                  ; allowances for process bootstrap,
                                  ; instead of hand-enumerating dyld
    (allow process-fork)
    (allow process-exec (subpath (param "VFS_ROOT")) ... /usr/bin,
                        CommandLineTools)
    (allow file-read*  (param VFS_ROOT/VFS_ROOT_ALT), /usr, /System,
                       /Library, /private)
    (allow file-write* (param WRITE_DIR/WRITE_DIR_ALT),
                       /private/var/folders)
    (deny network*)

Key moves to lift: (import "system.sb") kills the impossible
enumerate-everything game; (param ...) makes profiles reusable via
sandbox-exec -D; the *_ALT params handle macOS path duality
(/var vs /private/var canonicalization); reads confined to VFS
roots + OS paths = the read half of the mount ceiling, ALREADY
SOLVED.

## The allow-default variant: d3c2b328 (June 1)
vx-runner v2's simpler take: (allow default) + deny network* +
deny file-write* except /dev + declared write roots. Write-set
confinement only, WITH a passing host test (write-in-root ok,
sibling denied, reads work). Note it REGRESSED from Feb's
deny-default — recover why before re-choosing.

## Discipline finding
This archaeology was ALREADY DONE on Jul 3 (c7b405b1, banked in
docs/design/vix-synthesis.md "seatbelt SBPL gen") — and both the
head agent and today's recon lost it. Design docs are memory only
if the recon pass reads them; vix-synthesis.md must be on the
recon reading list alongside the resurrection note.

## Consequence for the integration plan
macOS is NOT greenfield and not even half-built — the deny-default
profile shape is done (f4effa0a). Work remaining: lift it into the
one-generator design (mounts → params), swap static system paths
for mount-derived ones where the VFS serves toolchains, and wire
the WRITE_DIR flush to CAS. Recovery: git show f4effa0a d3c2b328.
