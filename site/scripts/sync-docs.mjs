// docs/*.md を site のコンテンツへ生成する。
//
// - 正本は repo 直下の docs/ と scripts/docs-meta.json（title / description）のみ。
//   site/src/content/docs/*.md は生成物のためコミットしない（.gitignore 参照）。
//   例外は index.mdx（トップページ。docs/ に正本を持たない手書きファイル）。
// - 対象は docs/ 直下の .md のみ。サブディレクトリ（issues/ plan/ webui/ 等）は
//   公開範囲の判断が必要なため対象外。
// - 先頭の H1 は frontmatter の title と重複するため落とす。
// - 本文中の `](../<repo relative path>)` は GitHub の絶対 URL へ書き換える。
//   公開サイトは `/egopulse` 配下でビルドされ、リポジトリのソースを配信しないため、
//   相対リンクのままでは 404 になる。ディレクトリは blob ではなく tree を指す。
//   画像・参照定義などインラインリンク以外の形は書き換え対象外とし、残存チェックで
//   同期を失敗させる。
// - `npm run build` / `npm run dev` の前に自動実行される。単体実行は `npm run sync`。
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import path from "node:path";

const REPO_ROOT = path.resolve(import.meta.dirname, "../..");
const REPO_DOCS = path.join(REPO_ROOT, "docs");
const SITE_DOCS = path.resolve(import.meta.dirname, "../src/content/docs");
const META_PATH = path.resolve(import.meta.dirname, "docs-meta.json");

const SOURCE_URL = "https://github.com/endo-ly/egopulse";
const SOURCE_REF = "main";

const meta = JSON.parse(readFileSync(META_PATH, "utf8"));

// 先頭の `!` は画像。インラインリンク `[text](../path)` のリンク先だけを対象にする。
const MARKDOWN_LINK = /(!?)\[([^\]\n]*)\]\((\.\.\/[^)\s]*)\)/g;

/** `../<repo relative path>[#fragment]` を GitHub の絶対 URL へ変換する。 */
function toSourceUrl(target, page, lineNumber) {
  const [repoPath, fragment] = splitFragment(target.slice("../".length));
  const normalized = repoPath.replace(/\/+$/, "");
  const stat = statSync(path.join(REPO_ROOT, normalized), { throwIfNoEntry: false });
  if (stat === undefined) {
    throw new Error(
      `${page}:${lineNumber}: link target does not exist in the repository: ${target}`,
    );
  }
  const kind = stat.isDirectory() ? "tree" : "blob";
  return `${SOURCE_URL}/${kind}/${SOURCE_REF}/${normalized}${fragment}`;
}

function splitFragment(target) {
  const hashAt = target.indexOf("#");
  return hashAt === -1 ? [target, ""] : [target.slice(0, hashAt), target.slice(hashAt)];
}

/** 本文中のリポジトリ相対リンクを GitHub の絶対 URL へ書き換える。 */
function rewriteSourceLinks(body, page, lineOffset) {
  return body
    .split("\n")
    .map((line, index) =>
      line.replace(MARKDOWN_LINK, (link, imageFlag, text, target) =>
        imageFlag === "!"
          ? link
          : `[${text}](${toSourceUrl(target, page, index + 1 + lineOffset)})`,
      ),
    )
    .join("\n");
}

// 相対パスがリポジトリを指す表記（インラインリンク・参照定義）。
const REPO_RELATIVE_TARGET = /\](?:\(|:)\s*\.\.\//;

// 画像・タイトル付きリンク・参照定義など、上の書き換え対象外の形で追加された相対リンクは
// 公開物で 404 になるため、同期自体を失敗させる。
function assertNoRepoRelativeLinks(body, page, lineOffset) {
  const offenders = body
    .split("\n")
    .map((line, index) => [index + 1 + lineOffset, line.trim()])
    .filter(([, line]) => REPO_RELATIVE_TARGET.test(line))
    .map(([lineNumber, line]) => `  ${page}:${lineNumber}: ${line}`);
  if (offenders.length > 0) {
    throw new Error(
      "repository-relative links left unresolved " +
        "(only inline links [text](../path/to/file) are rewritten):\n" +
        offenders.join("\n"),
    );
  }
}

for (const entry of readdirSync(REPO_DOCS, { withFileTypes: true })) {
  if (!entry.isFile() || !entry.name.endsWith(".md")) continue;
  const source = readFileSync(path.join(REPO_DOCS, entry.name), "utf8");
  // 先頭の H1 は frontmatter の title と重複するため落とす
  const h1 = source.match(/^# .+\n+/)?.[0] ?? "";
  // エラーで報告する行番号は、生成後ではなく元の docs/*.md の行に合わせる
  const lineOffset = h1.split("\n").length - 1;
  const body = rewriteSourceLinks(source.slice(h1.length), entry.name, lineOffset);
  assertNoRepoRelativeLinks(body, entry.name, lineOffset);
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
