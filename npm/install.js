#!/usr/bin/env node

"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const { execSync } = require("child_process");

const REPO = "portless-rs/portless";
const BIN_DIR = path.join(__dirname, "bin");
const BIN_PATH = path.join(BIN_DIR, "portless");

function getPlatformTarget() {
  const platform = process.platform;
  const arch = process.arch;

  if (platform === "darwin" && arch === "arm64") return "aarch64-apple-darwin";
  if (platform === "darwin" && arch === "x64") return "x86_64-apple-darwin";
  if (platform === "linux" && arch === "x64") return "x86_64-unknown-linux-gnu";
  if (platform === "linux" && arch === "arm64") return "aarch64-unknown-linux-gnu";

  throw new Error(
    `Unsupported platform: ${platform} ${arch}.\n` +
    `Please install portless via cargo: cargo install portless\n` +
    `Or download manually from: https://github.com/${REPO}/releases`
  );
}

function getVersion() {
  const pkg = JSON.parse(fs.readFileSync(path.join(__dirname, "package.json"), "utf8"));
  return pkg.version;
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    const follow = (url) => {
      https.get(url, { headers: { "User-Agent": "portless-rs-npm-installer" } }, (res) => {
        if (res.statusCode === 301 || res.statusCode === 302) {
          return follow(res.headers.location);
        }
        if (res.statusCode !== 200) {
          return reject(new Error(`Download failed: HTTP ${res.statusCode} for ${url}`));
        }
        const file = fs.createWriteStream(dest);
        res.pipe(file);
        file.on("finish", () => file.close(resolve));
        file.on("error", reject);
      }).on("error", reject);
    };
    follow(url);
  });
}

function extractTarGz(archivePath, destDir) {
  // Use system tar — available on macOS and all Linux distros
  execSync(`tar -xzf "${archivePath}" -C "${destDir}"`, { stdio: "inherit" });
}

async function main() {
  const target = getPlatformTarget();
  const version = getVersion();
  const archiveName = `portless-${target}.tar.gz`;
  const tmpArchive = path.join(BIN_DIR, archiveName);

  console.log(`Installing portless v${version} for ${target}...`);

  if (!fs.existsSync(BIN_DIR)) {
    fs.mkdirSync(BIN_DIR, { recursive: true });
  }

  try {
    const url = `https://github.com/${REPO}/releases/download/v${version}/${archiveName}`;
    await download(url, tmpArchive);
    extractTarGz(tmpArchive, BIN_DIR);
    fs.unlinkSync(tmpArchive);
    fs.chmodSync(BIN_PATH, 0o755);
    console.log(`portless installed successfully -> ${BIN_PATH}`);
  } catch (err) {
    // Clean up partial download
    if (fs.existsSync(tmpArchive)) fs.unlinkSync(tmpArchive);
    console.error(`\nFailed to install portless: ${err.message}`);
    if (err.message.includes("404")) {
      console.error(`\nThe GitHub release v${version} may not exist or has no prebuilt binary for ${target}.`);
      console.error(`Either wait for a new release or install from source:`);
    }
    console.error(`  cargo install portless`);
    console.error(`Or download from: https://github.com/${REPO}/releases`);
    process.exit(1);
  }
}

main();
