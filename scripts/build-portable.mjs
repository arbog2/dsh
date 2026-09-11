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

const sources = [
  join(releaseDir, "deepseek-harness.exe"),
  join(releaseDir, "node.exe"),
  join(releaseDir, "runtime"),
  join(releaseDir, "tools"),
];
for (const source of sources) {
  if (!existsSync(source)) {
    throw new Error(`Release artifact is missing: ${source}`);
  }
}

rmSync(stagingDir, { recursive: true, force: true });
mkdirSync(stagingDir, { recursive: true });
cpSync(sources[0], join(stagingDir, "DeepSeekHarness.exe"));
cpSync(sources[1], join(stagingDir, "node.exe"));
cpSync(sources[2], join(stagingDir, "runtime"), { recursive: true });
cpSync(sources[3], join(stagingDir, "tools"), { recursive: true });

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
