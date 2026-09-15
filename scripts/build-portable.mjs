import { spawnSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  renameSync,
  rmSync,
  statSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const releaseDir = join(root, "src-tauri", "target", "release");
const outputDir = join(root, "DeepSeekHarness");
const stagingDir = join(root, ".DeepSeekHarness-staging");
const archive = join(root, "DeepSeekHarness-portable-x64.zip");

const environment = {
  ...process.env,
  PATH: `${join(homedir(), ".cargo", "bin")};${process.env.PATH ?? ""}`,
};

runPnpm(["tauri", "build", "--no-bundle"], environment);

// The bundle is assembled from the source tree rather than from whatever Tauri
// left in the release directory. Tauri copies resources incrementally and never
// prunes ones that were dropped from the configuration, so reading them back
// would silently ship artifacts from earlier builds.
const executables = [[join(releaseDir, "deepseek-harness.exe"), "DeepSeekHarness.exe"]];
for (const [source] of executables) {
  if (!existsSync(source)) {
    throw new Error(`Release artifact is missing: ${source}`);
  }
}
const resources = [
  [join(root, "runtime", "node", "node.exe"), "node.exe", false],
  [join(root, "runtime", "pnpm"), join("runtime", "pnpm"), true],
  [join(root, "runtime", "git"), join("runtime", "git"), true],
  [join(root, "scripts", "repair-runtime.mjs"), join("tools", "repair-runtime.mjs"), false],
];
for (const [source] of resources) {
  if (!existsSync(source)) {
    throw new Error(`Bundle resource is missing: ${source}`);
  }
}

// The bundled seed archives are intentionally absent: the first run clones and
// builds Harness from GitHub instead of unpacking a shipped baseline.
for (const name of ["harness-source.zip", "harness-runtime.zip"]) {
  if (existsSync(join(root, "runtime", name))) {
    console.log(`Note: runtime/${name} exists locally but is not packaged.`);
  }
}

rmSync(stagingDir, { recursive: true, force: true });
mkdirSync(stagingDir, { recursive: true });
for (const [source, destination] of executables) {
  cpSync(source, join(stagingDir, destination));
}
for (const [source, destination, recursive] of resources) {
  const target = join(stagingDir, destination);
  mkdirSync(dirname(target), { recursive: true });
  cpSync(source, target, { recursive });
}

rmSync(outputDir, { recursive: true, force: true });
renameSync(stagingDir, outputDir);
rmSync(archive, { force: true });
run("tar.exe", ["-a", "-c", "-f", archive, "DeepSeekHarness"], root, environment);

const exe = join(outputDir, "DeepSeekHarness.exe");
console.log(`Portable executable: ${exe}`);
console.log(`Portable archive: ${archive}`);
console.log(`Executable size: ${formatMiB(statSync(exe).size)} MiB`);
console.log(`Archive size: ${formatMiB(statSync(archive).size)} MiB`);

function runPnpm(args, env) {
  const command = process.platform === "win32" ? "pnpm.cmd" : "pnpm";
  run(command, args, root, env, true);
}

function run(command, args, cwd, env, shell = false) {
  const result = spawnSync(command, args, {
    cwd,
    env,
    shell,
    stdio: "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    throw new Error(`Command failed (${result.status}): ${command} ${args.join(" ")}`);
  }
}

function formatMiB(bytes) {
  return (bytes / 1024 / 1024).toFixed(1);
}
