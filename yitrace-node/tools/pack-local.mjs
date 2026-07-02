import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const dist = join(root, "dist");
const npmDir = join(root, "npm");
const localPlatform =
  {
    "darwin:arm64": "darwin-arm64",
    "darwin:x64": "darwin-x64",
    "linux:arm64": "linux-arm64-gnu",
    "linux:x64": "linux-x64-gnu",
    "win32:x64": "win32-x64-msvc",
  }[`${process.platform}:${process.arch}`] ?? null;

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

if (localPlatform) {
  const binary = `yitrace-db.${localPlatform}.node`;
  const from = join(root, binary);
  const toDir = join(npmDir, localPlatform);
  if (existsSync(from) && existsSync(join(toDir, "package.json"))) {
    copyFileSync(from, join(toDir, binary));
  }
}

function npmPack(args, cwd = root) {
  execFileSync("npm", ["pack", ...args, "--pack-destination", dist], {
    cwd,
    stdio: "inherit",
  });
}

npmPack([]);

for (const name of readdirSync(npmDir)) {
  const dir = join(npmDir, name);
  if (!existsSync(join(dir, "package.json"))) continue;
  if (!readdirSync(dir).some((file) => file.endsWith(".node"))) continue;
  npmPack([dir]);
}

console.log(`\nPacked local yiTrace npm artifacts into ${dist}`);
