#!/usr/bin/env node
// Zero-dependency dev server for the browser example.
// Browsers block ES-module/WASM loading over file://, so we need HTTP.
//
// Usage:
//   node serve.js          # serves on http://localhost:8080
//   node serve.js 3000     # custom port
"use strict";
const http = require("http");
const fs   = require("fs");
const path = require("path");

const PORT = Number(process.argv[2]) || 8080;
const ROOT = __dirname;

const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js":   "text/javascript; charset=utf-8",
  ".mjs":  "text/javascript; charset=utf-8",
  ".wasm": "application/wasm",
  ".dic":  "application/octet-stream",
  ".css":  "text/css; charset=utf-8",
};

http.createServer((req, res) => {
  // Strip query string and decode URI
  const urlPath = decodeURIComponent(req.url.split("?")[0]);
  const filePath = path.join(ROOT, urlPath === "/" ? "index.html" : urlPath);

  // Prevent path traversal outside ROOT
  if (!filePath.startsWith(ROOT)) {
    res.writeHead(403);
    res.end("Forbidden");
    return;
  }

  fs.readFile(filePath, (err, data) => {
    if (err) {
      res.writeHead(err.code === "ENOENT" ? 404 : 500);
      res.end(err.message);
      return;
    }
    const ext = path.extname(filePath);
    res.writeHead(200, { "Content-Type": MIME[ext] ?? "application/octet-stream" });
    res.end(data);
  });
}).listen(PORT, () => {
  console.log(`Sudachi browser demo → http://localhost:${PORT}`);
  console.log("Press Ctrl+C to stop.");
});
