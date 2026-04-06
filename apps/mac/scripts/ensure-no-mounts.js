#!/usr/bin/env node
import { execSync } from "child_process";

function run(cmd, { ignoreError = false } = {}) {
  try {
    return execSync(cmd, { encoding: "utf-8" }).trim();
  } catch (err) {
    if (!ignoreError) {
      console.error(`Command failed: ${cmd}`);
      console.error(err.stderr || err.message);
      process.exit(1);
    }
    return null;
  }
}

console.log("Ensuring no Vixen mounts or VixenFS processes remain...");

// 1. Unmount the known mountpoint (ignore if not mounted)
console.log("Unmounting /tmp/vx/vfs (if mounted)...");
run("umount /tmp/vx/vfs", { ignoreError: true });

// 2. Find and unmount any stale vixen:// mountpoints
console.log("Checking for remaining vixen:// mountpoints...");
const mountOutput = run("mount", { ignoreError: true }) || "";
for (const line of mountOutput.split("\n")) {
  if (line.includes("vixen://")) {
    const match = line.match(/on\s+(.+?)\s+\(/);
    if (match) {
      const mountPath = match[1];
      console.log(`Found vixen:// mount at ${mountPath}, unmounting...`);
      run(`umount "${mountPath}"`, { ignoreError: true });
    }
  }
}

// 3. Wait for any VixenFS processes to exit naturally
console.log("Waiting for VixenFS processes to exit (if any)...");
for (let i = 0; i < 10; i++) {
  const pgrepOutput = run("pgrep -x VixenFS", { ignoreError: true });
  if (!pgrepOutput) {
    console.log("VixenFS exited.");
    break;
  }
  console.log(`VixenFS still running (attempt ${i + 1}/10), waiting 200ms...`);
  run("sleep 0.2", { ignoreError: true });
}
const stillRunning = run("pgrep -x VixenFS", { ignoreError: true });
if (stillRunning) {
  console.log("Warning: VixenFS is still running after 2s timeout.");
}

console.log("Cleanup complete.");
