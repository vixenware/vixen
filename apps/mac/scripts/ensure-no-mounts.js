#!/usr/bin/env node
import { execSync } from "child_process";

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
  success: "✅",
  warning: "⚠️",
  info: "ℹ️",
  search: "🔍",
  clean: "🧹",
  mount: "📀",
};

function colorize(color, text) {
  return `${colors[color]}${text}${colors.reset}`;
}

function run(cmd, { ignoreError = false, stdio = undefined } = {}) {
  try {
    return execSync(cmd, { encoding: "utf-8", stdio }).trim();
  } catch (err) {
    if (!ignoreError) {
      console.error(`Command failed: ${cmd}`);
      console.error(err.stderr || err.message);
      process.exit(1);
    }
    return null;
  }
}

console.log(`${emojis.clean} Ensuring no Vixen mounts or VixenFS processes remain...`);

// 1. Find and unmount any active vixenfs mountpoints
console.log(`${emojis.search} Scanning for vixenfs mountpoints...`);
const mountOutput = run("mount", { ignoreError: true }) || "";
let unmounted = 0;
for (const line of mountOutput.split("\n")) {
  if (line.includes("(vixenfs,")) {
    const match = line.match(/on\s+(.+?)\s+\(/);
    if (match) {
      const mountPath = match[1];
      console.log(`${emojis.mount} Unmounting vixenfs at ${colorize("cyan", mountPath)}...`);
      run(`umount "${mountPath}"`, { ignoreError: true, stdio: "pipe" });
      unmounted++;
    }
  }
}
if (unmounted === 0) {
  console.log(`${emojis.success} No vixenfs mounts found.`);
}

// 3. Wait for any VixenFS processes to exit naturally
console.log(`${emojis.info} Waiting for VixenFS processes to exit (if any)...`);
for (let i = 0; i < 10; i++) {
  const pgrepOutput = run("pgrep -x VixenFS", { ignoreError: true });
  if (!pgrepOutput) {
    console.log(`${emojis.success} VixenFS exited.`);
    break;
  }
  console.log(`${emojis.warning} VixenFS still running (attempt ${colorize("cyan", `${i + 1}/10`)}, waiting 200ms...`);
  run("sleep 0.2", { ignoreError: true });
}
const stillRunning = run("pgrep -x VixenFS", { ignoreError: true });
if (stillRunning) {
  console.log(`${emojis.warning} ${colorize("yellow", "Warning: VixenFS is still running after 2s timeout.")}`);
}

console.log(`\n${emojis.success} ${colorize("green", "Cleanup complete.")}`);
