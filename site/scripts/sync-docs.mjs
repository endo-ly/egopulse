// docs/*.md を site のコンテンツへ生成する。
//
// - 正本は repo 直下の docs/ と scripts/docs-meta.json（title / description）のみ。
//   site/src/content/docs/*.md は生成物のためコミットしない（.gitignore 参照）。
//   例外は index.mdx（トップページ。docs/ に正本を持たない手書きファイル）。
// - 対象は docs/ 直下の .md のみ。サブディレクトリ（issues/ plan/ webui/ 等）は
//   公開範囲の判断が必要なため対象外。
// - 先頭の H1 は frontmatter の title と重複するため落とす。
// - `npm run build` / `npm run dev` の前に自動実行される。単体実行は `npm run sync`。
import { existsSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";

const REPO_DOCS = path.resolve(import.meta.dirname, "../../docs");
const SITE_DOCS = path.resolve(import.meta.dirname, "../src/content/docs");
const META_PATH = path.resolve(import.meta.dirname, "docs-meta.json");

const meta = JSON.parse(readFileSync(META_PATH, "utf8"));

for (const entry of readdirSync(REPO_DOCS, { withFileTypes: true })) {
  if (!entry.isFile() || !entry.name.endsWith(".md")) continue;
  const source = readFileSync(path.join(REPO_DOCS, entry.name), "utf8");
  // 先頭の H1 は frontmatter の title と重複するため落とす
  const body = source.replace(/^# .+\n+/, "");
  const frontmatter = meta[entry.name] ?? {
    title: entry.name.replace(/\.md$/, ""),
    description: entry.name.replace(/\.md$/, ""),
  };
  if (!(entry.name in meta)) {
    console.warn(`no meta (using filename as title): ${entry.name}`);
  }
  const dest = path.join(SITE_DOCS, entry.name);
  writeFileSync(
    dest,
    `---\ntitle: ${JSON.stringify(frontmatter.title)}\n` +
      `description: ${JSON.stringify(frontmatter.description)}\n---\n\n` +
      body,
  );
  console.log(`synced: ${entry.name}`);
}

for (const name of Object.keys(meta)) {
  if (!existsSync(path.join(REPO_DOCS, name))) {
    console.warn(`stale meta (no source in docs/): ${name}`);
  }
}
