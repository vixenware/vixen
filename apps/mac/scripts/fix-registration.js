#!/usr/bin/env node

import { spawnSync } from "child_process";
import { existsSync } from "fs";
import { join, resolve } from "path";

// ANSI color codes
const colors = {
  reset: "\x1b[0m",
  bold: "\x1b[1m",
  red: "\x1b[31m",
  green: "\x1b[32m",
  yellow: "\x1b[33m",
  blue: "\x1b[34m",
  cyan: "\x1b[36m",
};

const emojis = {
  error: "❌",
  success: "✅",
  warning: "⚠️",
  info: "ℹ️",
  tool: "🔧",
  bundle: "📦",
  search: "🔍",
  keep: "✔️",
};

function colorize(color, text) {
  return `${colors[color]}${text}${colors.reset}`;
}

const lsRegisterPath =
  "/System/Library/Frameworks/CoreServices.framework/Versions/Current/Frameworks/LaunchServices.framework/Versions/Current/Support/lsregister";

const bundleURL = resolve(process.argv[2]);
if (!bundleURL) {
  console.error(`${emojis.error} Usage: fix-registration.js <path-to-Vixen.app>`);
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
  console.error(`${emojis.error} VixenFS extension not found in:`);
  for (const c of candidates) {
    console.error(`  ${c}`);
  }
  process.exit(1);
}

console.log(`${emojis.bundle} Repairing VixenFS registration toward: ${colorize("cyan", desiredPath)}`);

function runTool(cmd, args) {
  console.log(`${emojis.tool} ${cmd} ${colorize("cyan", args.join(" "))}`);
  const result = spawnSync(cmd, args, { encoding: "utf-8" });
  if (result.stdout?.trim()) console.log(result.stdout.trimEnd());
  if (result.stderr?.trim()) console.error(result.stderr.trimEnd());
  if (result.status !== 0) {
    console.error(`  ${emojis.error} exited with status ${result.status}`);
  }
  return result;
}

function currentRegisteredPaths() {
  const result = spawnSync(
    "/usr/bin/pluginkit",
    ["-mAvvv", "-i", "eu.bearcove.vixenfs"],
    { encoding: "utf-8" }
  );
  // Silent check

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
let changed = false;
if (before.length > 0) {
  console.log(`${emojis.info} Current VixenFS registrations: ${colorize("cyan", before.join(", "))}`);
}

for (const stalePath of before) {
  if (resolve(stalePath) === desiredPath) {
    console.log(`${emojis.keep} Keeping: ${colorize("cyan", stalePath)}`);
    continue;
  }
  console.log(`${emojis.warning} Unregistering: ${colorize("cyan", stalePath)}`);
  runTool("/usr/bin/pluginkit", ["-r", stalePath]);
  changed = true;
}

// 2. Register the correct extension
if (before.some((path) => resolve(path) === desiredPath)) {
  console.log(`${emojis.keep} Already registered: ${colorize("cyan", desiredPath)}`);
} else {
  console.log(`${emojis.success} Registering: ${colorize("cyan", desiredPath)}`);
  runTool("/usr/bin/pluginkit", ["-a", desiredPath]);
  changed = true;
}

// 3. Run lsregister on the app bundle, same flags as VixenApp.swift
if (changed && existsSync(lsRegisterPath)) {
  runTool(lsRegisterPath, ["-f", "-R", "-trusted", bundleURL]);
} else if (!changed) {
  console.log(`${emojis.keep} Registration already clean`);
} else {
  console.warn(`${emojis.warning} lsregister not found, skipping`);
}

// 4. Verify
const after = currentRegisteredPaths();
if (after.includes(desiredPath)) {
  console.log(`\n${emojis.success} VixenFS registration complete: ${colorize("cyan", desiredPath)}`);
} else {
  console.error(`\n${emojis.error} VixenFS registration failed.`);
  console.error(`  Desired: ${colorize("green", desiredPath)}`);
  console.error(`  Current: ${colorize("red", after.join(", ") || "(none)")}`);
  process.exit(1);
}
