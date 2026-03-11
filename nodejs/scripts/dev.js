#!/usr/bin/env node

import { execSync } from "child_process";
import { existsSync, mkdirSync, copyFileSync, writeFileSync } from "fs";
import { join, dirname } from "path";
import { fileURLToPath } from "url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);

const projectRoot = join(__dirname, "..", "..");
const nodejsDir = join(__dirname, "..");

function run(command, cwd = projectRoot) {
  console.log(`Running: ${command}`);
  try {
    execSync(command, { cwd, stdio: "inherit" });
  } catch (error) {
    console.error(`Failed to run: ${command}`);
    process.exit(1);
  }
}

function getTarget() {
  const platform = process.platform;
  const arch = process.arch;

  const targets = {
    "linux-x64": "x86_64-unknown-linux-musl",
    "linux-arm64": "aarch64-unknown-linux-musl",
    "darwin-x64": "x86_64-apple-darwin",
    "darwin-arm64": "aarch64-apple-darwin",
    "win32-x64": "x86_64-pc-windows-msvc",
    "win32-arm64": "aarch64-pc-windows-msvc",
  };

  const key = `${platform}-${arch}`;
  const target = targets[key];

  if (!target) {
    console.error(`Unsupported platform: ${platform} (${arch})`);
    process.exit(1);
  }

  return target;
}

function main() {
  const command = process.argv[2] || "build";
  const target = getTarget();

  console.log(`\n🎯 Target platform: ${target}`);
  console.log(`📦 Command: ${command}\n`);

  switch (command) {
    case "build":
      buildBinary(target);
      break;
    case "vendor":
      vendorBinary(target);
      break;
    case "test":
      testBinary(target);
      break;
    case "all":
      buildBinary(target);
      vendorBinary(target);
      testBinary(target);
      break;
    default:
      console.error(`Unknown command: ${command}`);
      console.error("Usage: node dev.js [build|vendor|test|all]");
      process.exit(1);
  }
}

function buildBinary(target) {
  console.log("🔨 Building Rust binary...");
  
  run(`rustup target add ${target}`);
  run(`cargo build --release --target ${target} -p savfox-cli`);
  
  console.log("✅ Build complete!\n");
}

function vendorBinary(target) {
  console.log("📦 Vendoring binary to nodejs folder...");
  
  const vendorDir = join(nodejsDir, "vendor", target, "savfox");
  mkdirSync(vendorDir, { recursive: true });
  
  const binaryName = process.platform === "win32" ? "savfox.exe" : "savfox";
  const sourcePath = join(
    projectRoot,
    "target",
    target,
    "release",
    binaryName
  );
  const destPath = join(vendorDir, binaryName);
  
  if (!existsSync(sourcePath)) {
    console.error(`❌ Binary not found at: ${sourcePath}`);
    console.error("   Run 'node dev.js build' first");
    process.exit(1);
  }
  
  copyFileSync(sourcePath, destPath);
  
  if (process.platform !== "win32") {
    run(`chmod +x "${destPath}"`);
  }
  
  console.log(`✅ Binary vendored to: ${destPath}\n`);
}

function testBinary() {
  console.log("🧪 Testing binary...\n");
  
  const wrapperPath = join(nodejsDir, "bin", "savfox.js");
  
  if (!existsSync(wrapperPath)) {
    console.error(`❌ Wrapper not found at: ${wrapperPath}`);
    process.exit(1);
  }
  
  run(`node "${wrapperPath}" --version`, nodejsDir);
  run(`node "${wrapperPath}" --help`, nodejsDir);
  
  console.log("\n✅ Tests passed!\n");
}

main();
