import { build } from "esbuild";
import { cpSync, mkdirSync, existsSync, rmSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));
const root = join(__dirname, "..");
const isChrome = process.argv.includes("--chrome");
const dist = join(root, isChrome ? "dist-chrome" : "dist");
const staticDir = join(root, "static");
const sudachiPkg = join(root, "node_modules", "@f3liz", "sudachi-wasm", "pkg");
const sudachiDict = join(root, "dict", "system_core.xdic");
const target = isChrome ? ["chrome116"] : ["firefox115"];

// Clean
if (existsSync(dist)) rmSync(dist, { recursive: true });

// Create directories
mkdirSync(join(dist, "wasm"), { recursive: true });
mkdirSync(join(dist, "dict"), { recursive: true });

// Bundle content script (IIFE — injected into web pages)
await build({
  entryPoints: [join(root, "src", "content.ts")],
  bundle: true,
  outfile: join(dist, "content.js"),
  format: "iife",
  target,
  logLevel: "info",
});

// Bundle background script (ESM — loaded as service worker / background.html)
await build({
  entryPoints: [join(root, "src", "background.ts")],
  bundle: true,
  outfile: join(dist, "background.js"),
  format: "esm",
  target,
  logLevel: "info",
});

// Bundle popup script (IIFE — runs in popup context)
await build({
  entryPoints: [join(root, "src", "popup.ts")],
  bundle: true,
  outfile: join(dist, "popup.js"),
  format: "iife",
  target,
  logLevel: "info",
});

// Copy static files
const staticFiles = isChrome
  ? ["popup.html", "content.css"]
  : ["background.html", "popup.html", "content.css"];

for (const file of staticFiles) {
  const src = join(staticDir, file);
  if (existsSync(src)) {
    cpSync(src, join(dist, file));
  }
}

// Copy icons
const iconsDir = join(staticDir, "icons");
if (existsSync(iconsDir)) {
  cpSync(iconsDir, join(dist, "icons"), { recursive: true });
  console.log("✅ Copied icons");
}

// Copy manifest
const manifestSrc = join(
  staticDir,
  isChrome ? "manifest.chrome.json" : "manifest.json",
);
cpSync(manifestSrc, join(dist, "manifest.json"));

// Copy WASM files
if (existsSync(sudachiPkg)) {
  for (const file of ["sudachi_wasm.js", "sudachi_wasm_bg.wasm"]) {
    const src = join(sudachiPkg, file);
    if (existsSync(src)) {
      cpSync(src, join(dist, "wasm", file));
    }
  }
  console.log("✅ Copied WASM files from @f3liz/sudachi-wasm");
} else {
  console.warn("⚠️  @f3liz/sudachi-wasm not found at:", sudachiPkg);
}

// Copy dictionary (large file, only if present)
if (existsSync(sudachiDict)) {
  cpSync(sudachiDict, join(dist, "dict", "system_core.xdic"));
  console.log("✅ Copied dictionary (system_core.xdic)");
} else {
  console.warn(
    "⚠️  Dictionary not found at:",
    sudachiDict,
    "\n   Run 'npm run fetch-dict' to download system_core.xdic.",
  );
}

console.log(`\n🎉 Build complete! (${isChrome ? "Chrome/Chromium" : "Firefox"})`);
if (isChrome) {
  console.log("   Run:  npx web-ext run -s dist-chrome --target chromium");
} else {
  console.log("   Run:  npx web-ext run -s dist");
}
