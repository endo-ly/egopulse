# Plan: `run_update()` の reqwest ベース自己更新へのリプレイ

`curl | bash` による install.sh 外部依存を廃止し、reqwest で GitHub Releases API を直接叩いて tar.gz をダウンロード・展開・atomic replace する自己更新に切り替える。

> **Note**: 以下の具体的なコード例・API 設計・構成（How）はあくまで参考である。実装時によりよい設計方針があれば積極的に採用すること。

## 設計方針

- **reqwest で完結**: 外部コマンド（curl/wget/bash）への依存を排除。HTTP クライアントは既存の reqwest を利用
- **`CARGO_PKG_REPOSITORY` からURL導出**: `repository` フィールド `"https://github.com/endo-ly/egopulse"` から API URL を組み立てる。リポジトリ名変更に追従
- **Atomic binary replace**: `current_exe → .old`, `new → current_exe` の rename ベース。失敗時は rollback。cross-device 対応（copy → rename）
- **既存の `restart_service()` はそのまま**: systemd 再起動ロジックは変更なし
- **エラーハンドリング**: 既存の `EgoPulseError::Internal` で統一。新しいバリアントは追加しない（スコープ外）

## Plan スコープ

WT作成 → 実装 → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 実装元 |
|---|---|
| `src/gateway.rs` — `run_update()` | 既存関数をリプレイ |
| `src/gateway.rs` — 新規ヘルパー関数群 | 新規追加 |
| `Cargo.toml` | `flate2`, `tar` 追加（既存依存にないため） |
| `src/gateway.rs` — テスト | 新規追加 |

---

## Step 0: Worktree 作成

`/review-self-update-reqwest` ブランチで WT 作成。

---

## Step 1: Cargo.toml に tar/flate2 追加

### 実装

`Cargo.toml` に以下を追加:
- `tar = "0.4"`
- `flate2 = "1.0"`

（reqwest, tempfile, serde, serde_json は既存依存に含まれる）

### コミット

`chore: add tar and flate2 dependencies for self-update`

---

## Step 2: update モジュールのヘルパー関数群

### 実装

`src/gateway.rs` 内に private 関数として実装（独立モジュール化はスコープ外、既存の gateway に同居）:

1. **`repo_api_path()`** — `CARGO_PKG_REPOSITORY` から `"endo-ly/egopulse"` を抽出
2. **`fetch_latest_release(client) -> ReleaseInfo`** — GitHub API `/repos/{path}/releases/latest` を叩いて JSON を取得。`ReleaseInfo` は `{ tag_name: String, assets: Vec<Asset> }` の最小構造体（関数内で serde_json::Value をパースでも可）
3. **`resolve_asset_url(assets, target) -> Option<String>`** — OS/Arch にマッチする tar.gz の `browser_download_url` を返す。target triple は `env!("TARGET")` または `cfg!` マクロで組み立て
4. **`download_and_extract(client, url) -> Result<PathBuf>`** — tar.gz を tempfile にダウンロード → 展開 → バイナリパスを返す
5. **`replace_binary(new_binary, current_exe) -> Result<()>`** — atomic replace（copy to staged → rename current → .old → rename staged → current、失敗時 rollback）

### コミット

`feat: add self-update helpers (fetch, resolve, download, replace)`

---

## Step 3: `run_update()` のリプレイ

### 実装

既存の `run_update()` を新しいヘルパー群を使って書き直す:

```
1. println version
2. reqwest::Client 构建
3. fetch_latest_release → tag_name を取得
4. バージョン比較（latest == current → "Already up to date" で終了）
5. resolve_asset_url → URL を取得（見つからなければエラー）
6. download_and_extract → 一時ディレクトリにバイナリを展開
7. replace_binary → 実行中バイナリを置換
8. println "Update completed"
9. restart_service()
```

### コミット

`refactor: rewrite run_update() to use reqwest-based self-update`

---

## Step 4: 動作確認

- `cargo fmt --check`
- `cargo check -p egopulse`
- `cargo clippy -p egopulse --all-targets --all-features -- -D warnings`
- `cargo test -p egopulse`

---

## Step 5: PR 作成

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
|---|---|---|
| `Cargo.toml` | 変更 | `tar`, `flate2` 追加 |
| `src/gateway.rs` | 変更 | `run_update()` リプレイ + ヘルパー関数追加 |

---

## コミット分割

1. `chore: add tar and flate2 dependencies for self-update`
2. `feat: add self-update helpers (fetch, resolve, download, replace)`
3. `refactor: rewrite run_update() to use reqwest-based self-update`

---

## テストケース一覧

TDD なし（ユーザー指示により省略）。手動動作確認のみ。

---

## 工数見積もり

| Step | 内容 | 見積もり |
|---|---|---|
| Step 1 | Cargo.toml 変更 | ~5 行 |
| Step 2 | ヘルパー関数群 | ~150 行 |
| Step 3 | run_update() リプレイ | ~40 行 |
| **合計** | | **~195 行** |
