#!/usr/bin/env node

import { execFileSync } from "child_process";
import { statSync } from "fs";

function unregisterOne() {
  let output = execFileSync("pluginkit", ["-mAvvv"], { encoding: "utf-8" })
  let entries = output.split("\n\n");

  for (let e of entries) {
    if (!e.includes("vixenfs")) {
      continue;
    }

    let prefix = "Path = ";
    let pathLine = e.split("\n").map(l => l.trim()).find(l => l.startsWith(prefix));
    if (!pathLine) {
      continue;
    }

    let extPath = pathLine.substring(prefix.length).trim();
    let stats = statSync(extPath);
    if (!stats.isDirectory()) {
      throw new Error(`Weird, a registered extension is not a dir? ${extPath}`);
    }
    console.log(`Unregistering ${pathLine}`);
    let output = execFileSync(`pluginkit`, [`-r`, extPath], { encoding: "utf-8" });
    console.log(`Output:\n =====\n${output}\n=====`);
    return true;
  }

  return false; // did not unregister any
}

while (true) {
  if (!unregisterOne()) {
    console.log("All done!");
    process.exit(0);
  }
}
