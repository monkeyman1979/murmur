import { readdir, readFile } from "node:fs/promises";
import { join } from "node:path";

async function javascriptFiles(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  const nested = await Promise.all(entries.map(async (entry) => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return javascriptFiles(path);
    return entry.isFile() && path.endsWith(".js") ? [path] : [];
  }));
  return nested.flat();
}

const forbidden = [
  /\bconsole\s*\./,
  /\bdebugger\b/,
];

for (const file of await javascriptFiles("dist")) {
  const source = await readFile(file, "utf8");
  for (const pattern of forbidden) {
    if (pattern.test(source)) {
      throw new Error(`production diagnostic token ${pattern} found in ${file}`);
    }
  }
}

console.log("Production bundle contains no console or debugger statements.");
