#!/usr/bin/env node

import { execFileSync, spawnSync } from "child_process";
import { existsSync } from "fs";
import { join } from "path";

const lsRegisterPath =
  "/System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister";

const bundleURL = process.argv[2];
if (!bundleURL) {
  console.error("Usage: fix-registration.js <path-to-Vixen.app>");
  process.exit(1);
}

// Mirror VixenApp.swift: check Contents/Extensions first, then Contents/PlugIns
const name = "VixenFS.appex";
const candidates = [
  join(bundleURL, "Contents", "Extensions", name),
  join(bundleURL, "Contents", "PlugIns", name),
];
const desiredPath = candidates.find((p) => existsSync(p));
if (!desiredPath) {
  console.error(
    `VixenFS extension not found in:\n${candidates.join("\n")}`
  );
  process.exit(1);
}

console.log(`Repairing VixenFS registration toward: ${desiredPath}`);

function runTool(cmd, args) {
  console.log(`+ ${cmd} ${args.join(" ")}`);
  const result = spawnSync(cmd, args, { encoding: "utf-8" });
  if (result.stdout?.trim()) console.log(result.stdout.trimEnd());
  if (result.stderr?.trim()) console.error(result.stderr.trimEnd());
  if (result.status !== 0) {
    console.error(`  (exited with status ${result.status})`);
  }
  return result;
}

function currentRegisteredPaths() {
  const result = spawnSync(
    "/usr/bin/pluginkit",
    ["-mAvvv", "-i", "eu.bearcove.vixenfs"],
    { encoding: "utf-8" }
  );

  const output = result.stdout ?? "";
  // Extract every .appex path from the output, deduplicated
  const matches = [...output.matchAll(/\/[^\s]+\.appex/g)];
  const seen = new Set();
  for (const m of matches) {
    seen.add(m[0]);
  }
  return [...seen];
}

// 1. Unregister stale registrations (anything that is NOT the desired path)
const before = currentRegisteredPaths();
if (before.length > 0) {
  console.log(`Current VixenFS registrations: ${before.join(", ")}`);
}

for (const stalePath of before) {
  if (stalePath === desiredPath) {
    console.log(`Keeping (already correct): ${stalePath}`);
    continue;
  }
  console.log(`Unregistering stale registration: ${stalePath}`);
  runTool("/usr/bin/pluginkit", ["-r", stalePath]);
}

// 2. Register the correct extension
runTool("/usr/bin/pluginkit", ["-a", desiredPath]);

// 3. Run lsregister on the app bundle, same flags as VixenApp.swift
if (existsSync(lsRegisterPath)) {
  runTool(lsRegisterPath, ["-f", "-R", "-trusted", bundleURL]);
} else {
  console.warn(`lsregister not found at ${lsRegisterPath}, skipping`);
}

// 4. Verify
const after = currentRegisteredPaths();
if (after.includes(desiredPath)) {
  console.log(`\nVixenFS registration now includes bundled appex: ${desiredPath}`);
} else {
  console.error(
    `\nVixenFS registration still does not point at bundled appex.\nCurrent registrations: ${after.join(", ") || "(none)"}`
  );
  process.exit(1);
}
