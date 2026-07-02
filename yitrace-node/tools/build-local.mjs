import { execFileSync } from "node:child_process";

const target =
  process.env.NAPI_TARGET ??
  ({
    "darwin:arm64": "aarch64-apple-darwin",
    "darwin:x64": "x86_64-apple-darwin",
    "linux:arm64": "aarch64-unknown-linux-gnu",
    "linux:x64": "x86_64-unknown-linux-gnu",
    "win32:x64": "x86_64-pc-windows-msvc",
  }[`${process.platform}:${process.arch}`]);

if (!target) {
  throw new Error(`Unsupported local build target: ${process.platform}/${process.arch}`);
}

execFileSync("napi", ["build", "--release", "--platform", "--target", target, ...process.argv.slice(2)], {
  stdio: "inherit",
});
