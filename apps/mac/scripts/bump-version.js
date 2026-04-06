import { readFileSync, writeFileSync } from "fs";

let path = "./dev-version.txt";

try {
  const current = readFileSync(path, { encoding: "utf-8" }).trim();
  if (!current) throw new Error('Version file is empty.');

  const parts = current.split('.');
  if (parts.length !== 3) throw new Error('Version format is invalid.');

  parts[2] = (parseInt(parts[2], 10) + 1).toString();
  const newVersion = parts.join('.');

  writeFileSync(path, newVersion, { encoding: "utf-8" });
  console.log(`Version updated from ${current} to ${newVersion}`);
} catch (error) {
  console.error(`Error updating version: ${error.message}`);
}
