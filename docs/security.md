# EgoPulse セキュリティガード

LLM エージェントによるシークレット窃取を防ぐ多層防御。ツール実行の前・実行時・後にそれぞれ独立した検査層を配置し、単一層の突破で全体が崩れない設計。

## 全体構成

```text
ツール実行要求
    │
    ▼
┌─────────────────────────────────┐
│  事前検査                        │
│  ├ コマンド検閲  (bash のみ)     │
│  └ パス検閲     (全ツール)       │
├─────────────────────────────────┤
│  ツール実行                      │
├─────────────────────────────────┤
│  事後処理                        │
│  └ 出力リダクション (全ツール)    │
└─────────────────────────────────┘
    │
    ▼
  結果をエージェントに返却
```

---

## 事前検査

### コマンド検閲

**対象**: `bash` ツール  
**実装**: [command_guard.rs](../../egopulse/src/tools/command_guard.rs)

コマンド文字列を実行前に検査し、シークレット露出の原因となるコマンドをブロックする。

#### ブロック対象

| パターン | 理由 | 回避手段 |
|---|---|---|
| `env` | 環境変数の全ダンプ | `echo $VAR_NAME` で個別参照 |
| `printenv` | 同上 | 同上 |
| `set`（引数なし） | シェル変数・関数の全ダンプ | `set -e` 等のオプション付きは許可 |
| `/proc/self/*` | プロセス内部情報（environ, mem, maps, fd 等）の読み取り | なし |
| `/proc/<pid>/*` | 他プロセスの内部情報読み取り | なし |

#### 検出方法

- コマンド文字列を `|`, `;`, `&&`, `||`, 改行で分割し、各セグメントの先頭コマンドを照合
- クォート内の文字列は照合対象外
- `bash -c` / `sh -c` / `eval` などのシェル実行コンテキストでは、トークン境界で `env` / `printenv` を追加検査
- `/proc/self/`・`/proc/<数値>/` はファイル名に関わらず包括ブロック

### パス検閲

**対象**: `bash`, `read`, `write`, `edit`, `grep`, `find`, `ls`  
**実装**: [path_guard.rs](../../egopulse/src/tools/path_guard.rs)

ファイルツールと bash ツールの両方で、機密パスへのアクセスをブロックする。

#### ブロック対象

**ディレクトリ**: `.ssh`, `.aws`, `.gnupg`, `.kube`, `.config/gcloud`

**ファイル**:

| ファイル | 種別 |
|---|---|
| `.env`, `.env.local`, `.env.production`, `.env.development` | 環境変数ファイル |
| `credentials`, `credentials.json` | クラウド認証情報 |
| `token.json` | OAuth トークン |
| `secrets.yaml`, `secrets.json` | シークレット定義 |
| `id_rsa`, `id_ed25519`, `id_ecdsa`, `id_dsa`（公開鍵含む） | SSH 鍵 |
| `.netrc`, `.npmrc` | 認証情報付き設定 |

**絶対パス**: `/etc/shadow`, `/etc/gshadow`, `/etc/sudoers`, `/proc/self/*`, `/proc/<pid>/*`

※ `/proc/cpuinfo`, `/proc/meminfo` 等の数値以外のエントリはシステム情報のため許可。

#### 検証機能

- **パス正規化**: `..` によるトラバーサルを正規化してから検査
- **symlink 検証**: 各パスコンポーネントのメタデータを確認し、シンボリックリンクを検出したらブロック（`/tmp`, `/var` は例外）
- **`/proc/` 包括チェック**: `/proc/self/` または `/proc/<数値>/` をコマンド経由・ファイルツール経由を問わずブロック

#### 適用箇所

| ツール | チェック関数 |
|---|---|
| `bash` | `check_command_paths()` — コマンド文字列内のパスを検査（機密ファイルに加えて機密ディレクトリ/サブパスも検出） |
| `read` / `write` / `edit` | `check_path()` — ファイルパスを検査 |
| `grep` | `check_path()` + `rg --glob` 除外 — 検索ルート検査に加えて機密配下（`.ssh` / `.env` / `.config/gcloud` 等）の再帰検索を除外 |
| `find` / `ls` | `check_path()` — 検索パスを検査 |

---

## 事後処理

### 出力リダクション

**対象**: 全ツール（MCP ツール含む）  
**実装**: [mod.rs](../../egopulse/src/tools/mod.rs)

ツール実行結果にシークレットが含まれる場合、自動的にマスクして返却する。事前検査をすり抜けた場合の最終防衛線。

#### 値ベースリダクション

起動時に Config から収集したシークレット値と完全一致する文字列を `[REDACTED:<キー名>]` に置換する。

- 収集対象: `providers.*.api_key`, `channels.*.auth_token`, `channels.*.file_auth_token`, `channels.*.bot_token`, `channels.*.file_bot_token`
- 8 文字未満の値はスキップ（誤検出防止）
- 長い値から順に置換（部分一致の誤検出防止）

#### パターンベースリダクション

Well-known なシークレットプレフィックスに一致する文字列を `[REDACTED:secret]` に置換する。シークレット境界は空白・引用符・改行・セミコロンで判定。

| プレフィックス | サービス |
|---|---|
| `sk-` | OpenAI |
| `sk-or-` | OpenRouter |
| `sk-ant-` | Anthropic |
| `xoxb-` | Slack Bot Token |
| `xapp-` | Slack App Token |
| `ghp_` | GitHub PAT |
| `gho_` | GitHub OAuth |
| `ghu_` | GitHub User-to-Server |
| `ghs_` | GitHub Server-to-Server |
| `github_pat_` | GitHub Fine-grained PAT |
| `glpat-` | GitLab PAT |
| `AKIA` | AWS Access Key ID |
| `ASIA` | AWS Temporary Access Key ID |
| `AIza` | Google API Key / OAuth |
| `sk_live_` | Stripe Live Secret Key |
| `sk_test_` | Stripe Test Secret Key |
| `rk_live_` | Stripe Live Restricted Key |

---

## 制約事項

| 制約 | 影響 | 回避手段 |
|---|---|---|
| `env`/`printenv`/`set` のブロック | 環境変数の一覧確認が不可 | `echo $VAR_NAME` で個別参照 |
| `/proc/self/*` のブロック | プロセスメモリ・FD・メモリマップ等の読み取り不可 | なし |
| `/proc/<pid>/*` のブロック | 他プロセスの内部情報読み取り不可 | なし |
| `.env` 系のパスガード | `cat .env` での直接読み取り不可 | 値はツール経由で注入済み |
| 出力リダクション | シークレット値が結果に含まれない | `[REDACTED:KEY_NAME]` として表示 |
| 値ベースリダクションの対象外 | Config に登録されていないシークレットはマスクされない | パターンベースで補完 |
