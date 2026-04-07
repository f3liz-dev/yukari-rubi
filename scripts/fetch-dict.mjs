import { createWriteStream, existsSync, mkdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { get } from "node:https";

const __dirname = dirname(fileURLToPath(import.meta.url));
const dictDir = join(__dirname, "..", "dict");
const dictPath = join(dictDir, "system_core.xdic");
const url =
  "https://github.com/f3liz-dev/sudachi.rs/releases/download/v0.1.7/system_core.xdic";

if (existsSync(dictPath)) {
  console.log("Dictionary already exists at", dictPath);
  process.exit(0);
}

mkdirSync(dictDir, { recursive: true });

function download(url, dest) {
  return new Promise((resolve, reject) => {
    get(url, (res) => {
      if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
        download(res.headers.location, dest).then(resolve, reject);
        return;
      }
      if (res.statusCode !== 200) {
        reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        return;
      }
      const total = Number(res.headers["content-length"]) || 0;
      let downloaded = 0;
      const file = createWriteStream(dest);
      res.on("data", (chunk) => {
        downloaded += chunk.length;
        if (total) {
          const pct = ((downloaded / total) * 100).toFixed(1);
          process.stdout.write(`\rDownloading: ${pct}% (${(downloaded / 1e6).toFixed(1)}MB / ${(total / 1e6).toFixed(1)}MB)`);
        }
      });
      res.pipe(file);
      file.on("finish", () => {
        file.close();
        console.log("\nDone.");
        resolve();
      });
      file.on("error", reject);
    }).on("error", reject);
  });
}

console.log("Fetching dictionary from", url);
await download(url, dictPath);
