import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { docsRoot, sourceRoot } from "./content.mjs";

const root = resolve(docsRoot, "..");
execFileSync("cargo", ["run", "--quiet", "--example", "docgen"], {
  cwd: root,
  stdio: "inherit",
});
const benchmarks = execFileSync(
  "cargo",
  ["run", "--quiet", "--manifest-path", "docs/doc-utils/Cargo.toml"],
  { cwd: root },
);
mkdirSync(resolve(docsRoot, ".vitepress"), { recursive: true });
writeFileSync(resolve(docsRoot, ".vitepress/benchmarks.json"), benchmarks);
copyFileSync(
  resolve(root, "fatou.schema.json"),
  resolve(sourceRoot, "public/fatou.schema.json"),
);
copyFileSync(
  resolve(root, "scripts/fatou-installer.sh"),
  resolve(sourceRoot, "public/install"),
);
copyFileSync(
  resolve(root, "scripts/fatou-installer.ps1"),
  resolve(sourceRoot, "public/install.ps1"),
);
