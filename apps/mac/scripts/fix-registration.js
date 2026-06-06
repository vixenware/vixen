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

if (!process.argv[2]) {
  console.error(`${emojis.error} Usage: fix-registration.js <path-to-Vixen.app>`);
  process.exit(1);
}

const bundleURL = resolve(process.argv[2]);
const appBundleIdentifier = "eu.bearcove.vixen";

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

const runningAsRoot = process.geteuid?.() === 0;

console.log(`${emojis.bundle} Cleaning VixenFS registration toward: ${colorize("cyan", desiredPath)}`);

function runTool(cmd, args) {
  console.log(`${emojis.tool} ${cmd} ${colorize("cyan", args.join(" "))}`);
  const result = spawnSync(cmd, args, {
    encoding: "utf-8",
    maxBuffer: 128 * 1024 * 1024,
  });
  if (result.stdout?.trim()) console.log(result.stdout.trimEnd());
  if (result.stderr?.trim()) console.error(result.stderr.trimEnd());
  if (result.status !== 0) {
    console.error(`  ${emojis.error} exited with status ${result.status}`);
  }
  return result;
}

function runQuiet(cmd, args) {
  return spawnSync(cmd, args, {
    encoding: "utf-8",
    maxBuffer: 128 * 1024 * 1024,
  });
}

function currentRegisteredPaths() {
  const result = runQuiet("/usr/bin/pluginkit", [
    "-mAvvv",
    "-i",
    "eu.bearcove.vixenfs",
  ]);
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

function launchServicesFieldValue(value) {
  const trimmed = value.trim();
  return trimmed.replace(/ \(0x[0-9a-fA-F]+\)$/, "");
}

function currentRegisteredAppPaths() {
  if (!existsSync(lsRegisterPath)) {
    return null;
  }

  const result = runQuiet(lsRegisterPath, ["-dump"]);
  if (result.status !== 0) {
    console.warn(`${emojis.warning} Could not query LaunchServices registrations`);
    if (result.stderr?.trim()) console.error(result.stderr.trimEnd());
    return null;
  }

  const paths = [];
  const seen = new Set();
  let currentPath = null;
  let currentIdentifier = null;

  function finishRecord() {
    if (currentIdentifier !== appBundleIdentifier || !currentPath) return;
    if (!currentPath.endsWith("/Vixen.app")) return;
    const key = resolve(currentPath);
    if (seen.has(key)) return;
    seen.add(key);
    paths.push(currentPath);
  }

  for (const rawLine of (result.stdout ?? "").split(/\r?\n/)) {
    const line = rawLine.trim();
    if (line.startsWith("----")) {
      finishRecord();
      currentPath = null;
      currentIdentifier = null;
      continue;
    }
    if (line.startsWith("path:")) {
      currentPath = launchServicesFieldValue(line.slice("path:".length));
    } else if (line.startsWith("identifier:")) {
      currentIdentifier = launchServicesFieldValue(line.slice("identifier:".length));
    }
  }
  finishRecord();

  return paths;
}

function unregisterStaleApps(paths) {
  for (const stalePath of paths) {
    console.log(`${emojis.warning} Unregistering stale app: ${colorize("cyan", stalePath)}`);
    runTool(lsRegisterPath, ["-u", stalePath]);
  }
}

// 1. Unregister stale containing-app registrations. System Settings can rediscover
// stale embedded appex copies from these app records even when PlugInKit looks clean.
// When run as root, remove every explicit Vixen.app LaunchServices record. fskitd
// uses root LaunchServices state, and a manually added root record gets a distinct
// FSKit identity that has not been approved in System Settings.
const appPaths = currentRegisteredAppPaths();
const staleAppPaths = appPaths?.filter((path) => runningAsRoot || resolve(path) !== bundleURL) ?? [];
let changed = false;

if (appPaths && appPaths.length > 0) {
  console.log(`${emojis.info} Current Vixen.app registrations: ${colorize("cyan", appPaths.join(", "))}`);
}

unregisterStaleApps(staleAppPaths);
changed ||= staleAppPaths.length > 0;

// 2. Unregister stale extension registrations (anything that is NOT the desired path)
const before = currentRegisteredPaths();
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

// 3. Report whether automatic discovery has produced the desired extension record.
if (before.some((path) => resolve(path) === desiredPath)) {
  console.log(`${emojis.keep} Automatic registration present: ${colorize("cyan", desiredPath)}`);
} else {
  console.warn(`${emojis.warning} Desired VixenFS extension is not currently visible in PlugInKit.`);
  console.warn(`${emojis.info} Leaving discovery to macOS instead of manually adding a fresh FSKit identity.`);
}

// 4. Summarize. This helper intentionally does not call pluginkit -a or
// lsregister -f/-R/-trusted; those create new identities that FSKit may treat as
// disabled until the user approves them again.
if (!changed) {
  console.log(`${emojis.keep} Registration already clean`);
}

// 5. Verify
const after = currentRegisteredPaths();
if (after.includes(desiredPath)) {
  console.log(`\n${emojis.success} VixenFS registration clean: ${colorize("cyan", desiredPath)}`);
} else {
  console.warn(`\n${emojis.warning} VixenFS registration is not visible yet.`);
  console.error(`  Desired: ${colorize("green", desiredPath)}`);
  console.error(`  Current: ${colorize("red", after.join(", ") || "(none)")}`);
  console.warn(`  Open Vixen or System Settings and let macOS discover the installed app.`);
}
