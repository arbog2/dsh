import {
  cpSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";

const [sourceArg, targetArg] = process.argv.slice(2);
if (!sourceArg || !targetArg) {
  throw new Error("usage: repair-runtime.mjs <source-root> <runtime-root>");
}

const sourceRoot = resolve(sourceArg);
const targetRoot = resolve(targetArg);
const targetNodeModules = join(targetRoot, "node_modules");
if (!existsSync(targetNodeModules)) {
  throw new Error(`Runtime node_modules is missing at ${targetNodeModules}`);
}

const workspacePackages = indexWorkspacePackages(sourceRoot);
const queue = [];
const installed = new Set();

for (const packageName of workspacePackages.keys()) {
  if (existsSync(packagePath(targetNodeModules, packageName))) {
    installed.add(packageName);
    queue.push(packageName);
  }
}

while (queue.length > 0) {
  const packageName = queue.shift();
  if (!packageName || !installed.has(packageName)) continue;
  const manifest = readManifest(
    join(packagePath(targetNodeModules, packageName), "package.json"),
  );
  const required = new Set([
    ...Object.keys(manifest.dependencies ?? {}),
    ...Object.keys(manifest.peerDependencies ?? {}),
  ]);
  for (const dependency of required) {
    const sourceDirectory = workspacePackages.get(dependency);
    if (!sourceDirectory) continue;
    const destination = packagePath(targetNodeModules, dependency);
    if (!existsSync(destination)) {
      mkdirSync(dirname(destination), { recursive: true });
      const nestedNodeModules = join(sourceDirectory, "node_modules");
      cpSync(sourceDirectory, destination, {
        recursive: true,
        dereference: true,
        filter: (path) =>
          path !== nestedNodeModules && !path.startsWith(`${nestedNodeModules}\\`),
      });
    }
    if (!installed.has(dependency)) {
      installed.add(dependency);
      queue.push(dependency);
    }
  }
}

materializeLinks(targetNodeModules);
console.log(`Runtime dependency closure repaired with ${installed.size} workspace packages.`);

function indexWorkspacePackages(root) {
  const packages = new Map();
  const manifests = [join(root, "package.json")];
  for (const directory of childDirectories(join(root, "apps"))) {
    manifests.push(join(directory, "package.json"));
  }
  for (const group of childDirectories(join(root, "packages"))) {
    for (const directory of childDirectories(group)) {
      manifests.push(join(directory, "package.json"));
    }
  }
  for (const directory of childDirectories(join(root, "vendor"))) {
    manifests.push(join(directory, "package.json"));
  }
  manifests.push(
    join(root, "native", "system", "package.json"),
    join(root, "benchmarks", "package.json"),
    join(root, "website", "package.json"),
    join(root, "python", "sdk-runtime", "package.json"),
  );
  for (const directory of childDirectories(join(root, "native", "system", "packages"))) {
    manifests.push(join(directory, "package.json"));
  }

  for (const manifestPath of manifests) {
    if (!existsSync(manifestPath)) continue;
    const manifest = readManifest(manifestPath);
    if (typeof manifest.name === "string" && !packages.has(manifest.name)) {
      packages.set(manifest.name, dirname(manifestPath));
    }
  }
  return packages;
}

function childDirectories(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => join(directory, entry.name));
}

function packagePath(nodeModules, packageName) {
  return join(nodeModules, ...packageName.split("/"));
}

function readManifest(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function materializeLinks(nodeModules) {
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
