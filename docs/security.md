# EgoPulse セキュリティガード

LLM エージェントによるシークレット窃取を防ぐ多層防御。

## 目次

1. [全体構成](#1-全体構成)
2. [事前検査](#2-事前検査)
3. [事後処理](#3-事後処理)
4. [制約事項](#4-制約事項)
5. [Secret Mode 隔離戦略](#5-secret-mode-隔離戦略)
6. [Local API の trust boundary](#6-local-api-の-trust-boundary)
7. [Bash の隔離](#7-bash-の隔離)

---

## 1. 全体構成

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
│  └ bash の隔離  (bash のみ, §7)  │
├─────────────────────────────────┤
│  事後処理                        │
│  └ 出力リダクション (全ツール)    │
└─────────────────────────────────┘
    │
    ▼
  結果をエージェントに返却
```

---

## 2. 事前検査

遮断メッセージは方針の最終判断であることを明示し（`This is a policy decision; do not work around it. Ask the user if access is required.`）、値の読み出しなど回避経路の案内は行わない。エージェント側の規範としても、Core Instructions の Security rules に「遮断は最終判断。別構文・動的パス・スクリプト等での回避を試みず、ユーザーに何が遮断されたかを伝えて確認する」と定義されている。

1 Turn / 1 Pulse の `guard_denied` が 3 回に達すると、その Run の後続 Tool 実行をすべて停止する。次の model request では Tool 定義を渡さず、「遮断を最終判断として作業を打ち切り、ユーザーに報告する」runtime guard を注入する。LLM が Tool 要求を返した場合も実行層で拒否し、未実行要求は実行台帳へ記録しない。閾値到達は `warn` ログに記録される。Turn は最終報告を返し、Pulse は報告を Notify 出力としてホームチャネルへ送信する。Pulse が報告を生成できない場合や `PULSE_OK` を返した場合は、固定の停止通知を送信する。

### コマンド検閲

**対象**: `bash` ツール  
**実装**: [command_guard.rs](../../egopulse/src/tools/command_guard.rs)

コマンド文字列を実行前に検査し、シークレット露出の原因となるコマンドをブロックする。

#### ブロック対象

| パターン | 理由 | 回避手段 |
|---|---|---|
| `env` | 環境変数の全ダンプ | なし（必要な値は skill の `required_env` で注入） |
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
| `auth.json` | OAuth 認証情報 |
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
- **シェル文字列検査**: Bash の完全な構文解析は行わず、コメントを含むコマンド文字列全体をパス検査する。コメント中の機密語が誤検知される場合がある。エスケープ引用符やコマンド置換を含む文字列を、コメント扱いで検査対象から除外しない。

### Bash 環境

Bash は `--noprofile --norc -c` で起動し、ログイン設定や起動ファイルを読み込まない。親プロセスからは `PATH` / `HOME` / `LANG` / `TERM` のみを引き継ぎ、skill が宣言した `required_env` を追加する。`BASH_ENV` / `ENV` / `SHELLOPTS` / `BASHOPTS` は親環境・skill 環境のいずれからも渡さない。

#### 適用箇所

| ツール | チェック関数 |
|---|---|
| `bash` | `check_command_paths()` — コマンド文字列内のパスを検査（機密ファイルに加えて機密ディレクトリ/サブパスも検出） |
| `read` / `write` / `edit` | `check_path()` — ファイルパスを検査 |
| `grep` | `check_path()` + `rg --glob` 除外 — 検索ルート検査に加えて機密配下（`.ssh` / `.env` / `.config/gcloud` 等）の再帰検索を除外 |
| `find` / `ls` | `check_path()` — 検索パスを検査 |

---

## 3. 事後処理

### 出力リダクション

**対象**: 全ツール（MCP ツール含む）  
**実装**: [mod.rs](../../egopulse/src/tools/mod.rs)

ツール実行結果にシークレットが含まれる場合、自動的にマスクして返却する。事前検査をすり抜けた場合の最終防衛線。

#### 値ベースリダクション

起動時に Config から収集したシークレット値と完全一致する文字列を `[REDACTED:<キー名>]` に置換する。

- 収集対象（`ResolvedValue::value()` から取得）:
  - `providers.<provider>.api_key`
  - `channels.<channel>.auth_token`
  - `channels.<channel>.bots.<bot_id>.token`（Discord）
  - `channels.<channel>.telegram_bots.<bot_id>.token`（Telegram）
  - `webhooks.receivers.<receiver_id>.token`
  - `codex.bearer_token`（`openai-codex` プロバイダー使用時）
  - `mcp.<server>.headers.<key>` / `mcp.<server>.env.<key>`（`mcp.json` / `mcp.d` にある全 server 定義の解決後の値。接続に失敗した server や、後続の定義に上書きされた server も含む。`Authorization: Bearer <token>` は値全体に加えて `<token>` 単体も登録する）
  - `dotenv.<KEY>`（`.env` の全値。更新時刻でキャッシュし、変更はツール結果ごとに反映される）
  - `skill_env.<KEY>`（Turn に注入された skill の `required_env` の値）
- 秘密参照（`{ "source": "env" | "exec" }`）で宣言された値は、長さに関わらず登録する
- 参照を経ないリテラル（MCP の平文値、`.env` の全値）は、12 文字以上かつ Shannon エントロピー 3.0 bit/文字以上のものだけ登録する。`PORT=8080` や `true` のようなありふれた値で出力が崩れることを防ぐ。キー名が秘密を示すのに条件を満たさない `.env` の値は、キー名のみを起動時に `warn` に出す
- `file_token` / `file_auth_token` の YAML 表現は秘密値の正本として使わない
- 空文字列は登録しない（誤検出防止）
- 同じ値が複数経路に存在する場合は値で deduplicate し、置換結果を決定的にする
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

#### 文脈ベースリダクション

prefix を持たない値や Config に登録されていない値も、周囲の文脈が秘密の保持を示す場合に `[REDACTED:secret]` へ置換する。値ベース・prefix ベースの後に適用する。bash の標準出力にも同じ処理を適用する。

| 規則 | 検出する形 | 置換条件 |
|---|---|---|
| 代入形 | `KEY=VALUE`、`key: value`、`"key": "value"`。キー名が `api_key` / `secret` / `token` / `password` / `passwd` / `authorization` / `access_key` / `private_key` / `credential` を部分一致で含む（大文字小文字不問、`-` / `_` 区切り不問） | 値が 20 文字以上、Shannon エントロピー 3.5 bit/文字以上、lower / upper / digit のうち 2 種以上を含む |
| Authorization | `Authorization: Bearer / Basic / Token <値>` | 値が 8 文字以上 |
| Bearer | ヘッダー名を伴わない `Bearer <値>` | 値が 16 文字以上かつ文字クラス 2 種以上 |
| URL クエリ | `?` / `&` の後の `access_token` / `id_token` / `refresh_token` / `token` / `api_key` / `apikey` / `sig` / `signature` / `code` / `x-amz-signature` / `x-amz-credential` / `x-amz-security-token` | 値が 8 文字以上 |

- 代入形は、パス（`/`、`./`、`../`、`~` 始まり）、URL（`http(s)://` 始まり）、プレースホルダー（`$`、`{`、`*` 始まり、`REDACTED` を含む値）を置換しない。小文字だけの snake_case の単語（`authorization_code_flow` など）や数字だけの値も、文字クラスの条件で置換しない
- `ps eww` や環境変数ダンプのように `KEY=VALUE` が空白区切りで並ぶ出力は、代入形で 1 件ずつ検出される
- 偽陽性（秘密ではない値が伏せられる）は出力が欠けるだけで済むため許容し、偽陰性（漏洩）を避ける方向に倒す。秘密風の代入行を `read` した後にその行を `edit` する場合は、伏せられた値が `old_string` と一致しない

#### 保証範囲と限界

現時点の出力リダクションは、上記のとおり Config に登録された既知の秘密値と well-known パターンを対象とする。これは「ツール出力経由の偶発的・低レベルな秘密露出」に対する防御であり、Shell ツールの OS sandbox ではない。

- Shell ツール（`bash` / `write` / `edit`）によるプロセス外への秘密書き出しは、本リダクションでは防げない。OS レベルの隔離は現時点では未実施である。
- LLM への Prompt Injection により Agent が秘密値を加工・断片化して出力する経路を完全に封止する保証はない。値ベース・prefix ベース・文脈ベースのリダクションは可能な限り広く覆うが、完全な exfiltration 防止を主張しない。
- ツール実行そのものを禁止・隔離する境界は事前検査（コマンド・パス検閲）で担う。OS sandbox は現時点では未実施である。

---

## 4. 制約事項

| 制約 | 影響 | 回避手段 |
|---|---|---|
| `env`/`printenv`/`set` のブロック | 環境変数の一覧確認が不可 | なし（必要な値は skill の `required_env` で注入） |
| bash 子プロセスの環境変数 | 親の環境変数は引き継がれない（`PATH` / `HOME` / `LANG` / `TERM` と skill の `required_env` のみ） | skill の `required_env` で必要な値を宣言する |
| `/proc/self/*` のブロック | プロセスメモリ・FD・メモリマップ等の読み取り不可 | なし |
| `/proc/<pid>/*` のブロック | 他プロセスの内部情報読み取り不可 | なし |
| `.env` 系のパスガード | `cat .env` での直接読み取り不可 | 値はツール経由で注入済み |
| 出力リダクション | シークレット値が結果に含まれない | `[REDACTED:KEY_NAME]` として表示 |
| Compaction Archive | 会話アーカイブにシークレットが含まれる | 三層 redaction（値・prefix・文脈）適用 + ファイルパーミッション `0600` |
| 値ベースリダクションの対象外 | Config・MCP・`.env` に存在しない秘密（他プロセスの環境変数など）はマスクされない | prefix ベース・文脈ベースで補完 |
| bash の隔離 | `.env` と `<state_root>/runtime/`（DB・バックアップ・会話アーカイブ・Local API のソケット）を bash から読めない | 方針メッセージが表示される。必要な値は skill の `required_env` で注入し、それ以外はユーザーに依頼する |
| MCP の秘密参照 | `mcp.json` の `env` / `exec` 参照は、workspace 側の `mcp.json` にも書ける。Runtime が解決した値を自前のエンドポイントへ送らせる意図的な迂回は防げない | §7 の対象外（意図的な迂回） |

---

## 5. Secret Mode 隔離戦略

秘匿会話（`secret: true` のチャネル）を通常の会話経路から物理的に隔離する多層防御。各層で独立に秘匿内容を排除する。内部では `ConversationScope::Secret` としてスコープ全体に伝播し、コンテキスト構築から turn 終了まで一貫した境界を保証する（[architecture.md §7.1](./architecture.md#71-conversationscopeストレージ境界) 参照）。

### 5.1 物理ファイル分離

| 項目 | Normal スコープ | Secret スコープ |
|---|---|---|
| DB ファイル | `egopulse.db` | `secret.db` |
| Compaction archive | `runtime/groups/` | `runtime/secret_groups/` |

同じプロセス内でも別の `Database` インスタンス（別 `Mutex<Connection>`）として動作する。クロスデータベーストランザクションは不要（1 turn は通常/秘密いずれか一方で完結）。

### 5.2 構造的保証

Sleep Batch・PULSE は `ConversationScope::Normal` の DB（`egopulse.db`）のみ参照し、`ConversationScope::Secret` の DB（`secret.db`）には接続しない。これは実装の省略ではなく構造的保証。スコープはコンテキスト構築時に決定され、コード経路が存在しないため、誤って秘匿内容を処理することはない。

- Secret スコープのチャットメッセージは `episodic.md` 等に昇格しない
- PULSE は Secret スコープのチャットで発火・投稿しない

### 5.3 ログ Redaction

Secret スコープの turn では `tracing` の span に内容フィールドを含めない:

- `info_span!("turn", agent_id, scope = "secret")` — `user_msg` 等の content フィールドを含めない
- tool 実行ログは `name` と `status` のみ
- LLM request/response ログは token 数やエラーメタ情報のみ

`scope = "secret"` フィールドが span に記録されるため、ログ検索で Secret スコープの turn を識別できる。

### 5.4 バックアップ

`secret.db` は `egopulse.db` と同一スケジュールでバックアップされる。世代管理も独立。ファイルパーミッションは `0600`。バックアップファイルの取扱いはユーザー運用責任。

### 5.5 既知の制限

- Tool 実行（`write`/`edit`/`bash`）による `secret.db` 外への書き出しは防げない
- `secret.db` の暗号化（SQLCipher）には未対応
- WebUI / TUI での秘密チャット表示には未対応

---

## 6. Local API の trust boundary

Runtime と TUI の間の Local API は、`<state_root>/runtime/egopulse.sock` の Unix Domain Socket で提供する。ソケットは Runtime 起動時に作成し、ファイルパーミッションを `0600` に設定する。

この API は同じ OS ユーザーのプロセスから利用できる Runtime 操作面であり、ネットワーク越しの認証境界ではない。ソケットへアクセスできるユーザーは、次の操作を実行できる。

- Session 一覧・履歴の読み取り
- Session に対する Agent Turn の実行
- Slash Command の実行
- Tool 実行中の follow-up 入力の登録

bash の子プロセスからは、§7 の隔離によりソケットが見えない（Linux で隔離が有効な場合）。

Local API の Turn は Session の Channel identity を維持するが、TUI Client が所有する応答は接続中の Client へ配送し、元の Channel Adapter へ自動送信しない。`state_root` は絶対パスで指定し、Runtime と TUI の endpoint 解決基準を一致させる。

---

## 7. Bash の隔離

**対象**: `bash`（Linux）  
**実装**: [bash_sandbox.rs](../../egopulse/src/tools/bash_sandbox.rs)

字句検査（§2）はスクリプトの作成や動的なパス組み立てで迂回できる。bash の子プロセスは bubblewrap (`bwrap`) の中で起動し、EgoPulse の私有データと秘密を OS レベルで見えなくする。

### 守る範囲

- **守る**: エージェントがふつうの手段（`cat`、`sqlite3`、スクリプト、`ps`）で EgoPulse の私有データと秘密に触れること
- **守らない**: 隔離を破ること自体を目的とした意図的な迂回（`systemd-run` や `docker` で隔離の外にプロセスを起動する、ディスク装置を直接読む、など）
- 隠した場所に触れると方針メッセージ（`This path is hidden from bash by EgoPulse's security policy. ...`）が見え、回避せずユーザーに依頼するよう促す

### 隠すもの

| 対象 | 場所 | 隠し方 |
|---|---|---|
| Runtime の私有ディレクトリ | `<state_root>/runtime/` | tmpfs で覆い、`HIDDEN_BY_POLICY.txt` だけを置いて読み取り専用にする。DB・secret DB・バックアップ・会話アーカイブ・Local API のソケット・後から作られるファイルをまとめて隠す |
| 秘密ファイル | 設定ファイルと同じディレクトリの `.env` | 方針メッセージを読み取り専用で重ねる（存在する場合のみ） |

設定ファイルと MCP の設定ファイルは隠さない。秘密は SecretRef で `.env` に置き、これらのファイルには参照だけを書く。他のツールが使う認証情報も隠さない。

### 仕組み

- ホストのファイルシステムはそのまま見せる。PID・ネットワーク・`/run` は分離しないため、`systemctl`、`journalctl`、tmux（呼び出しをまたいで残るセッションを含む）は通常どおり使える
- Capability は、Docker が root のコンテナに既定で与える組から `CAP_MKNOD` を除いたものに絞る。パッケージ管理などの通常の root 作業は動く。マウント操作（`CAP_SYS_ADMIN`）、他プロセスの検査（`CAP_SYS_PTRACE`）、ファイルハンドル経由の読み取り（`CAP_DAC_READ_SEARCH`）は持たないため、隠したマウントの解除、`/proc/<pid>/root` や `/proc/<pid>/fd` を経由した読み取り、より多くの Capability を持つプロセス（root で動く通常のサービスなど）の環境変数の読み取りは拒否される
- `bwrap` はシステムのディレクトリ（`/usr/bin`、`/usr/local/bin`、`/bin`）からだけ探す。`PATH` 上の偽の実行ファイルを掴まないため

### 隔離できない場合

起動時に、Linux であること、`bwrap` が見つかること、実際に起動できることを確かめる。満たさない場合は bash を隔離せずに実行し、次の方法で知らせる。

- 起動時の `warn` ログ（理由付き）
- `/api/status` の `bash_sandbox`（`{"enabled": false, "reason": "..."}`）と `egopulse gateway status` の表示

### 残るリスク

- `systemd-run` や `docker` など、隔離の外でプロセスを起動する手段を使った迂回
- ディスク装置を直接読む経路
- 設定を保存すると `.env` が置き換えられ、それ以前から動き続けているプロセス（tmux のセッションなど）からは新しい `.env` が見える
- 設定ファイル・MCP の設定ファイルに平文で書いた秘密の利用（出力リダクションはされる）
- ネットワークは制限しないため、ローカルで動く他サービスへのアクセス
- Capability を絞って起動されたプロセスの環境変数は、sandbox 側の Capability がそれを包含する場合に読める

