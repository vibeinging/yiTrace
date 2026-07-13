import { execFileSync } from "node:child_process";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = dirname(dirname(fileURLToPath(import.meta.url)));
const dist = join(root, "dist");
const npmDir = join(root, "npm");
const generatedAt = new Date().toISOString();
const rootPackage = JSON.parse(readFileSync(join(root, "package.json"), "utf8"));
const localPlatform =
  {
    "darwin:arm64": "darwin-arm64",
    "darwin:x64": "darwin-x64",
    "linux:arm64": "linux-arm64-gnu",
    "linux:x64": "linux-x64-gnu",
    "win32:x64": "win32-x64-msvc",
  }[`${process.platform}:${process.arch}`] ?? null;

function gitOutput(args) {
  try {
    return execFileSync("git", args, {
      cwd: root,
      encoding: "utf8",
      stdio: ["ignore", "pipe", "ignore"],
    }).trim();
  } catch {
    return null;
  }
}

function gitSucceeds(args) {
  try {
    execFileSync("git", args, { cwd: root, stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

function npm(args, options) {
  if (process.env.npm_execpath) {
    return execFileSync(process.execPath, [process.env.npm_execpath, ...args], options);
  }
  return execFileSync("npm", args, {
    ...options,
    shell: process.platform === "win32",
  });
}

function timestampLabel(value) {
  return value
    .replace(/[-:]/g, "")
    .replace(/\.\d{3}Z$/, "Z")
    .replace("T", "t")
    .toLowerCase();
}

function cleanLabel(value) {
  const label = value.replace(/[^A-Za-z0-9._-]+/g, "-").replace(/^-+|-+$/g, "");
  if (!label) {
    throw new Error("YITRACE_PACK_LABEL must contain at least one alphanumeric, '.', '_' or '-' character");
  }
  return label;
}

const gitCommit = gitOutput(["rev-parse", "HEAD"]);
const gitShortCommit = gitOutput(["rev-parse", "--short=12", "HEAD"]);
if (gitCommit) {
  // A build may rewrite tracked generated files with identical contents. Refresh
  // cached stat data so diff-index reports content changes, not stale mtimes.
  gitSucceeds(["update-index", "-q", "--refresh"]);
}
const gitDirty = gitCommit ? !gitSucceeds(["diff-index", "--quiet", "HEAD", "--"]) : false;
const packLabel = cleanLabel(
  process.env.YITRACE_PACK_LABEL ??
    (gitShortCommit
      ? `g${gitShortCommit}${gitDirty ? `-dirty-${timestampLabel(generatedAt)}` : ""}`
      : `local-${timestampLabel(generatedAt)}`),
);

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
  const output = npm(["pack", ...args, "--pack-destination", dist], {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "inherit"],
  });
  const packed = basename(output.trim().split(/\r?\n/).filter(Boolean).at(-1) ?? "");
  if (!packed.endsWith(".tgz")) {
    throw new Error(`Could not determine npm pack output from: ${output}`);
  }
  const immutableName = packed.replace(/\.tgz$/, `-${packLabel}.tgz`);
  renameSync(join(dist, packed), join(dist, immutableName));
  console.log(immutableName);
  return immutableName;
}

const artifacts = [
  {
    kind: "root",
    package: rootPackage.name,
    version: rootPackage.version,
    file: npmPack([]),
  },
];

for (const name of readdirSync(npmDir)) {
  const dir = join(npmDir, name);
  if (!existsSync(join(dir, "package.json"))) continue;
  if (!readdirSync(dir).some((file) => file.endsWith(".node"))) continue;
  const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf8"));
  artifacts.push({
    kind: "platform",
    platform: name,
    package: pkg.name,
    version: pkg.version,
    file: npmPack([dir]),
  });
}

writeFileSync(
  join(dist, "pack-manifest.json"),
  JSON.stringify(
    {
      generatedAt,
      label: packLabel,
      packageVersion: rootPackage.version,
      git: {
        commit: gitCommit,
        dirty: gitDirty,
      },
      artifacts,
    },
    null,
    2,
  ) + "\n",
);

console.log(`\nPacked local yiTrace npm artifacts into ${dist}`);
console.log(`Immutable tarball label: ${packLabel}`);
if (gitDirty && !process.env.YITRACE_PACK_LABEL) {
  console.warn("Git worktree is dirty; tarball label includes a timestamp because the commit alone is not immutable.");
}
