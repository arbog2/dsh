import { spawnSync } from "node:child_process";
import {
  copyFileSync,
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const runtime = join(root, "runtime");
const downloads = join(runtime, ".downloads");
const nodeVersion = "22.19.0";
const pnpmVersion = "11.7.0";
const minGitVersion = "2.55.0.5";
const gitUrl =
  `https://github.com/git-for-windows/git/releases/download/v2.55.0.windows.5/` +
  `MinGit-${minGitVersion}-64-bit.zip`;
const harnessUrl =
  "https://codeload.github.com/deepseek-ai/deepseek-harness/zip/refs/heads/master";
const pnpmRegistry = "https://registry.npmmirror.com";
const officialRegistry = "https://registry.npmjs.org";

mkdirSync(downloads, { recursive: true });

await prepareNode();
await preparePnpm();
await prepareGit();
await prepareHarnessSource();
await buildHarnessRuntime();
// The desktop shell no longer ships seed archives: the first run clones and
// builds Harness from GitHub, so only Node.js, pnpm, MinGit, and the sidecar
// travel inside the bundle. The prepared directories above stay on disk for
// local development and manual inspection.
writeFileSync(join(runtime, ".prepared"), `${new Date().toISOString()}\n`);

console.log("Runtime preparation complete.");

async function prepareNode() {
  const nodeDir = join(runtime, "node");
  const nodeExe = join(nodeDir, "node.exe");
  if (existsSync(nodeExe)) return;

  const archive = await download(
    [
      `https://nodejs.org/dist/v${nodeVersion}/node-v${nodeVersion}-win-x64.zip`,
      `https://npmmirror.com/mirrors/node/v${nodeVersion}/node-v${nodeVersion}-win-x64.zip`,
    ],
    `node-v${nodeVersion}-win-x64.zip`,
  );
  const extracted = join(downloads, `node-v${nodeVersion}-win-x64`);
  resetDirectory(extracted);
  extractZip(archive, extracted);
  rmSync(nodeDir, { recursive: true, force: true });
  mkdirSync(nodeDir, { recursive: true });
  const source = join(extracted, `node-v${nodeVersion}-win-x64`, "node.exe");
  copyFileSync(source, nodeExe);
  console.log(`Prepared Node.js ${nodeVersion}.`);
}

async function preparePnpm() {
  const pnpmDir = join(runtime, "pnpm");
  const pnpmCjs = join(pnpmDir, "pnpm.cjs");
  if (
    existsSync(pnpmCjs) &&
    existsSync(join(pnpmDir, "bin", "pnpm.mjs")) &&
    existsSync(join(pnpmDir, "dist", "pnpm.mjs"))
  ) {
    return;
  }

  const archive = await download(
    [
      `https://registry.npmjs.org/pnpm/-/pnpm-${pnpmVersion}.tgz`,
      `https://registry.npmmirror.com/pnpm/-/pnpm-${pnpmVersion}.tgz`,
    ],
    `pnpm-${pnpmVersion}.tgz`,
  );
  const extracted = join(downloads, `pnpm-${pnpmVersion}`);
  resetDirectory(extracted);
  extractTar(archive, extracted);
  rmSync(pnpmDir, { recursive: true, force: true });
  mkdirSync(pnpmDir, { recursive: true });
  cpSync(join(extracted, "package", "bin"), join(pnpmDir, "bin"), { recursive: true });
  cpSync(join(extracted, "package", "dist"), join(pnpmDir, "dist"), { recursive: true });
  writeFileSync(pnpmCjs, "#!/usr/bin/env node\nimport('./bin/pnpm.mjs')\n");
  writeFileSync(
    join(pnpmDir, "pnpm.cmd"),
    '@echo off\r\n"%~dp0..\\node\\node.exe" "%~dp0pnpm.cjs" %*\r\n',
  );
  console.log(`Prepared pnpm ${pnpmVersion}.`);
}

async function prepareGit() {
  const gitExe = join(runtime, "git", "cmd", "git.exe");
  if (existsSync(gitExe)) return;

  const archive = await download(
    [
      gitUrl,
      `https://mirrors.tuna.tsinghua.edu.cn/github-release/git-for-windows/git/LatestRelease/MinGit-${minGitVersion}-64-bit.zip`,
    ],
    `MinGit-${minGitVersion}-64-bit.zip`,
  );
  const gitDir = join(runtime, "git");
  rmSync(gitDir, { recursive: true, force: true });
  mkdirSync(gitDir, { recursive: true });
  extractZip(archive, gitDir);
  if (!existsSync(gitExe)) {
    throw new Error(`MinGit archive did not contain ${gitExe}`);
  }
  console.log(`Prepared MinGit ${minGitVersion}.`);
}

async function prepareHarnessSource() {
  const sourceDir = join(runtime, "harness-source");
  const legacyDir = join(runtime, "harness");
  if (!existsSync(sourceDir) && existsSync(legacyDir)) {
    renameSync(legacyDir, sourceDir);
  }
  if (!existsSync(join(sourceDir, "package.json"))) {
    const archive = await download(
      [
        harnessUrl,
        "https://github.com/deepseek-ai/deepseek-harness/archive/refs/heads/master.zip",
      ],
      "deepseek-harness-master.zip",
    );
    const extracted = join(downloads, "deepseek-harness-master");
    resetDirectory(extracted);
    extractZip(archive, extracted);
    const unpacked = join(extracted, "deepseek-harness-master");
    rmSync(sourceDir, { recursive: true, force: true });
    cpSync(unpacked, sourceDir, { recursive: true });
  }

  initializeGitRepository(sourceDir);
}

function initializeGitRepository(sourceDir) {
  const gitDir = join(sourceDir, ".git");
  if (!existsSync(gitDir)) {
    runGit(["init", "-b", "master"], sourceDir);
  }
  const head = spawnSync("git", ["rev-parse", "--verify", "HEAD"], {
    cwd: sourceDir,
    encoding: "utf8",
    stdio: "pipe",
  });
  if (head.status !== 0) {
    runGit(["config", "user.name", "DeepSeek Harness Builder"], sourceDir);
    runGit(["config", "user.email", "builder@localhost"], sourceDir);
    runGit(["add", "-A"], sourceDir);
    runGit(
      ["commit", "--no-verify", "-m", "Bundled DeepSeek Harness baseline"],
      sourceDir,
    );
  }
  const remotes = run("git", ["remote"], sourceDir, true);
  if (!remotes.stdout.split(/\r?\n/u).includes("origin")) {
    runGit(["remote", "add", "origin", "https://github.com/deepseek-ai/deepseek-harness.git"], sourceDir);
  }
}

async function buildHarnessRuntime() {
  const sourceDir = join(runtime, "harness-source");
  const targetDir = join(runtime, "harness-runtime");
  const entry = join(targetDir, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
  if (existsSync(entry)) {
    removeSourceNodeModules(sourceDir);
    return;
  }

  const nodeExe = join(runtime, "node", "node.exe");
  const pnpmCjs = join(runtime, "pnpm", "pnpm.cjs");
  const builtCli = join(sourceDir, "apps", "cli", "lib", "bin.js");
  const builtWeb = join(
    sourceDir,
    "apps",
    "web",
    "dist",
    "index.html",
  );
  const deployDependencies = join(
    sourceDir,
    "python",
    "sdk-runtime",
    "node_modules",
    "@deepseek-ai",
    "dsh",
  );
  if (!existsSync(builtCli) || !existsSync(builtWeb)) {
    run(
      nodeExe,
      [
        pnpmCjs,
        "install",
        "--frozen-lockfile",
        "--config.confirmModulesPurge=false",
        `--registry=${pnpmRegistry}`,
      ],
      sourceDir,
    );
    run(nodeExe, [pnpmCjs, "run", "clean"], sourceDir);
    run(nodeExe, [pnpmCjs, "run", "build"], sourceDir);
  } else if (!existsSync(deployDependencies)) {
    run(
      nodeExe,
      [
        pnpmCjs,
        "install",
        "--frozen-lockfile",
        "--config.confirmModulesPurge=false",
        `--registry=${pnpmRegistry}`,
      ],
      sourceDir,
    );
  } else {
    console.log("Reusing the existing Harness build artifacts and dependencies.");
  }

  rmSync(targetDir, { recursive: true, force: true });
  run(
    nodeExe,
    [
      pnpmCjs,
      "--filter",
      "dsh-python-runtime-closure",
      "deploy",
      "--legacy",
      "--prod",
      "--config.allow-unused-patches=true",
      "--config.node-linker=hoisted",
      "--config.auto-install-peers=true",
      "--config.link-workspace-packages=true",
      "--config.ignore-scripts=true",
      "--config.confirmModulesPurge=false",
      `--registry=${pnpmRegistry}`,
      targetDir,
    ],
    sourceDir,
  );
  restoreLegacyHoists(sourceDir, targetDir);
  run(
    process.execPath,
    [join(root, "scripts", "repair-runtime.mjs"), sourceDir, targetDir],
    root,
  );
  runDeployedPostInstall(nodeExe, targetDir);
  if (!existsSync(entry)) {
    throw new Error(`Deploy did not produce ${entry}`);
  }

  // The bundled source only needs tracked files and Git metadata. Dependencies
  // live in harness-runtime and are reinstalled on first update.
  removeSourceNodeModules(sourceDir);
  console.log("Prepared the built Harness runtime.");
}

function removeSourceNodeModules(sourceDir) {
  for (const entry of readdirSync(sourceDir, { withFileTypes: true })) {
    const path = join(sourceDir, entry.name);
    if (entry.name === ".git") continue;
    if (entry.isDirectory() && entry.name === "node_modules") {
      rmSync(path, { recursive: true, force: true });
      continue;
    }
    if (entry.isDirectory()) removeSourceNodeModules(path);
  }
}

function runDeployedPostInstall(nodeExe, targetDir) {
  const koffi = join(targetDir, "node_modules", "koffi");
  if (existsSync(join(koffi, "cnoke.cjs"))) {
    run(
      nodeExe,
      [join(koffi, "cnoke.cjs"), "-P", koffi, "-D", "src/koffi", "--prebuild", "--release"],
      koffi,
    );
  }

  const nodePty = join(targetDir, "node_modules", "node-pty");
  if (existsSync(join(nodePty, "scripts", "post-install.js"))) {
    run(nodeExe, ["scripts/post-install.js"], nodePty);
  }

  const subprocess = join(
    targetDir,
    "node_modules",
    "@deepseek-ai",
    "dsh-subprocess-local",
  );
  if (existsSync(join(subprocess, "scripts", "ensure-spawn-helper.mjs"))) {
    run(nodeExe, ["scripts/ensure-spawn-helper.mjs"], subprocess);
  }
}

function restoreLegacyHoists(sourceDir, targetDir) {
  const manifest = JSON.parse(readFileSync(join(targetDir, "package.json"), "utf8"));
  const sourceNodeModules = join(sourceDir, "python", "sdk-runtime", "node_modules");
  for (const dependency of Object.keys(manifest.dependencies ?? {}).sort()) {
    const destination = join(targetDir, "node_modules", dependency);
    if (existsSync(destination)) continue;
    const source = join(sourceNodeModules, dependency);
    if (!existsSync(source)) {
      throw new Error(`Deploy dependency ${dependency} is missing from ${source}`);
    }
    mkdirSync(dirname(destination), { recursive: true });
    const nestedNodeModules = join(source, "node_modules");
    cpSync(source, destination, {
      recursive: true,
      dereference: true,
      filter: (path) =>
        path !== nestedNodeModules && !path.startsWith(`${nestedNodeModules}\\`),
    });
  }
}

function materializeStagedLinks(targetDir) {
  const nodeModules = join(targetDir, "node_modules");
  let link = findSymlink(nodeModules);
  while (link !== undefined) {
    const segments = link.slice(nodeModules.length + 1).split(/[\\/]/u);
    const binIndex = segments.lastIndexOf(".bin");
    if (binIndex >= 0) {
      rmSync(join(nodeModules, ...segments.slice(0, binIndex + 1)), {
        recursive: true,
        force: true,
      });
    } else {
      const source = realpathSync(link);
      const nestedNodeModules = join(source, "node_modules");
      rmSync(link, { recursive: true, force: true });
      cpSync(source, link, {
        recursive: true,
        dereference: true,
        filter: (path) =>
          path !== nestedNodeModules && !path.startsWith(`${nestedNodeModules}\\`),
      });
    }
    link = findSymlink(nodeModules);
  }
}

function findSymlink(directory) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    const metadata = lstatSync(path);
    if (metadata.isSymbolicLink()) return path;
    if (metadata.isDirectory()) {
      const nested = findSymlink(path);
      if (nested !== undefined) return nested;
    }
  }
  return undefined;
}



async function download(urls, filename) {
  const destination = join(downloads, filename);
  if (existsSync(destination)) return destination;

  let lastError;
  for (const url of urls) {
    try {
      console.log(`Downloading ${url}`);
      const response = await fetch(url, {
        redirect: "follow",
        signal: AbortSignal.timeout(120_000),
      });
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}`);
      }
      writeFileSync(destination, Buffer.from(await response.arrayBuffer()));
      return destination;
    } catch (error) {
      lastError = error;
      console.warn(`Download failed from ${url}: ${error instanceof Error ? error.message : error}`);
      rmSync(destination, { force: true });
    }
  }
  throw new Error(`No download source succeeded for ${filename}: ${lastError}`);
}

function extractZip(archive, destination) {
  mkdirSync(destination, { recursive: true });
  run("tar.exe", ["-xf", archive, "-C", destination], root);
}

function extractTar(archive, destination) {
  mkdirSync(destination, { recursive: true });
  run("tar.exe", ["-xzf", archive, "-C", destination], root);
}

function runGit(args, cwd) {
  run("git", args, cwd);
}

function run(command, args, cwd, capture = false) {
  const result = spawnSync(command, args, {
    cwd,
    env: {
      ...process.env,
      CI: "1",
      COREPACK_ENABLE_DOWNLOAD_PROMPT: "0",
      npm_config_registry: process.env.npm_config_registry ?? pnpmRegistry,
      NPM_CONFIG_REGISTRY: process.env.NPM_CONFIG_REGISTRY ?? pnpmRegistry,
      npm_config_fetch_retries: "3",
      npm_config_fetch_timeout: "120000",
    },
    encoding: "utf8",
    stdio: capture ? "pipe" : "inherit",
  });
  if (result.error) throw result.error;
  if (result.status !== 0) {
    const stderr = capture ? result.stderr.trim() : "";
    throw new Error(
      `Command failed (${result.status}): ${command} ${args.join(" ")}${stderr ? `\n${stderr}` : ""}`,
    );
  }
  return {
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
  };
}

function resetDirectory(path) {
  rmSync(path, { recursive: true, force: true });
  mkdirSync(path, { recursive: true });
}
