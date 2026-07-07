// Set the agent version from a single argument (the git tag, with or without a
// leading "v"). CI runs this before building so the git tag is the sole source
// of truth:  node scripts/set-version.mjs v0.1.0
//
// tauri.conf.json has NO version field — Tauri v2 inherits it from Cargo.toml —
// so only two files carry a number, and both are set here.
import { readFileSync, writeFileSync } from "node:fs";

const version = (process.argv[2] ?? "").replace(/^v/, "");
if (!/^\d+\.\d+\.\d+/.test(version)) {
  console.error(`usage: set-version <version>  (got: "${process.argv[2] ?? ""}")`);
  process.exit(1);
}

const targets = [
  // The one that matters: env!("CARGO_PKG_VERSION") -> rigs.agent_version.
  { path: "src-tauri/Cargo.toml", re: /^version = "[^"]*"/m, rep: `version = "${version}"` },
  // Frontend package metadata (cosmetic), kept in sync.
  { path: "package.json", re: /"version":\s*"[^"]*"/, rep: `"version": "${version}"` },
];

for (const t of targets) {
  const src = readFileSync(t.path, "utf8");
  if (!t.re.test(src)) {
    console.error(`no version field found in ${t.path}`);
    process.exit(1);
  }
  writeFileSync(t.path, src.replace(t.re, t.rep));
  console.log(`set ${t.path} -> ${version}`);
}
