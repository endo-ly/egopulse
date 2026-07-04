# EgoPulse WebUI — Chat Tab

Chat タブは、選択中 agent との Web セッションでの対話、および他チャネル（discord / telegram / cli / tui / voice）セッションの read-only 監査を行うメインの対話画面。

## 1. レイアウト

```
┌─ Chat Tab ──────────────────────────────────────────┐
│ ┌─ Chat Header ─────────────────────────────────┐   │
│ │ Web Chat  [web]                                │   │
│ │ Started 2026-06-29 14:32 · 12 messages         │   │
│ │                              [Refresh] [⋯]      │   │
│ └────────────────────────────────────────────────┘   │
│                                                      │
│ ┌─ Timeline ────────────────────────────────────┐    │
│ │  [user]    メッセージ…                          │    │
│ │  [lyre]    応答…                                │    │
│ │                                                │    │
│ │  [lyre]    応答…                                │    │
│ │            ┌─ Tool: read file.ts (120ms) ──┐   │    │
│ │            │  ▸ expand                     │   │    │
│ │            └────────────────────────────────┘   │    │
│ │                                                │    │
│ │  [lyre] ◴ 応答生成中…                          │    │
│ │                                                │    │
│ └────────────────────────────────────────────────┘   │
│                                                      │
│ ┌─ Composer ────────────────────────────────────┐    │
│ │ /command suggest (optional)                    │    │
│ │ [textarea]                                     │    │
│ │                          [Enter to send] [Send]│    │
│ └────────────────────────────────────────────────┘    │
└──────────────────────────────────────────────────────┘
```

Chat Tab は3領域で構成される：

| 領域 | 役割 | 高さ |
|---|---|---|
| Chat Header | セッション情報・操作 | auto |
| Timeline | メッセージ履歴・ストリーミング応答 | 1fr（スクロール） |
| Composer | 入力欄・送信 | auto |

---

## 2. Chat Header

- session label（左寄せ・強調本文）+ channel badge
- session metadata：開始時刻・メッセージ数・（read-only の場合）その旨
- 操作：Refresh（アイコン）、OverflowMenu（rename / delete）
- read-only セッションでは OverflowMenu を隠し Refresh のみ残す

### Channel 別の metadata 表示

| Channel | 表示 |
|---|---|
| `web` | `Started {time} · {n} messages` |
| `discord` | `Discord · {chat_title or external_chat_id} · {n} messages · read-only` |
| `telegram` | `Telegram · {chat_title or external_chat_id} · {n} messages · read-only` |
| `cli` | `CLI session · {n} messages · read-only` |
| `tui` | `TUI session · {n} messages · read-only` |
| `voice` | `Voice session · {n} messages · read-only` |

`chat_title` が未設定の場合は `external_chat_id`（チャネル ID や DM 相手等）を等幅表示する。Discord のチャネル/スレッド/DM の別は chat_title 側で適切に設定されることを前提とし、UI 側では特別扱いしない。

session label が未設定（自動生成キー `session-...`）の場合は、最初のユーザーメッセージの先頭30字を label として表示する（読み取り専用）。編集は OverflowMenu から。

---

## 3. Timeline

### 3.1 構造とスクロール

- 縦方向のスクロール可能なコンテナ
- メッセージ間は一定の gap（中大スペース）
- 自動スクロール：新しいメッセージ追加時、**ユーザーが最下部付近にいる場合のみ** 下へ追従
- ユーザーが過去ログを読んでいる場合は追従せず、右下に "Jump to latest" ボタンをフロート表示

### 3.2 自動スクロール判定

- 最下部からの距離が一定値未満（画面高の 10% 程度）の場合に auto-follow と判定
- auto-follow 中でなければ "Jump to latest" ボタンを表示

### 3.3 メッセージ検索（Cmd+F）

Timeline 内キーワード検索機能。

- `Cmd/Ctrl+F` で検索バーを Timeline 右上に表示
- 入力するとマッチ箇所をハイライト（`warning-soft` 背景）
- `Enter` / `Shift+Enter` で次/前のマッチへジャンプ（スクロール）
- `Esc` で検索バーを閉じる
- マッチ件数を "N / M" 形式で表示
- 検索対象: 全メッセージの本文（Markdown レンダリング前のプレーンテキスト）。sender label・timestamp は対象外
- 大文字小文字を区別しない

### 3.4 空状態・ロード中

- 新規セッションでメッセージ0件：Timeline 中央に EmptyState（"Start a conversation with {agentName}"）
- 初回ロード：大 spinner 中央
- refetch：既存 timeline を維持し右上に小 spinner
- 失敗：EmptyState + retry

---

## 4. MessageBubble

### 4.1 Sender 別の配置とスタイル

| `sender_kind` | 配置 | 背景 | 備考 |
|---|---|---|---|
| `user` | 右寄せ | `accent-2-soft`（パープル系） | ユーザー入力 |
| `assistant` | 左寄せ | `panel`（85% opacity） | LLM 応答・Pulse 通知 |
| `system` | 中央（幅 60% 程度） | `panel-2` | システムメッセージ |
| `tool` | 左寄せ・assistant の下 | `panel-2` | ツール結果 |

- bubble の最大幅：`min(760px, 80%)`
- panel 相当の radius・shadow

### 4.2 Avatar

24×24 の円形アイコン。

- `assistant` / `system` / `tool`：agent アイコン（設定されていれば `icon.png` の縮小、なければ汎用 AgentIcon）
- `user`：UserIcon

### 4.3 Sender Label

- 小テキスト・強調
- `user` の sender_id が `user:web:default` 形式の場合は `"You"` と表示
- `assistant` の sender_id が agent_id の場合は agent ラベル（config の `agents.<id>.label`）。未設定なら agent_id
- Pulse 起源のメッセージは `"Pulse · {agentLabel}"`（詳細は §4.5）

### 4.4 Timestamp

- 小テキスト・muted
- 当日は `HH:mm`、それ以前は `MM/DD HH:mm`
- hover で title 属性に full timestamp

### 4.5 Pulse 通知の識別

Pulse によって Home Surface へ送信されたメッセージは、通常の assistant メッセージとして保存される（[pulse.md §10](../pulse.md#10-出力仕様)）。

`/api/history` レスポンスの各メッセージに `message_kind` フィールドを追加し、Pulse 由来のメッセージには `message_kind: "pulse_notification"` を付ける。バックエンド側で `messages` テーブルの `message_kind` カラム値を返す。

識別した Pulse メッセージは以下のように表示する：

- 通常 assistant bubble と同じレイアウト
- bubble の meta 領域に小アイコン + `"Pulse"` ラベル
- アイコン背景：アクセント色の半透明 pill
- tooltip に intention_id と started_at を表示
- メッセージ本文：通常通り Markdown レンダリング

### 4.6 Content Rendering

`react-markdown` + `remark-gfm` で Markdown をレンダリングする。

- 見出し・段落・リスト・テーブル・引用・リンク・画像：標準的な Markdown レンダリング
- code (inline)：等幅フォント・小サイズ・半透明黒背景・小 radius
- code block：[design-system.md §9.9](./design-system.md#99-code-block) に従う。Copy ボタン必須。**20行を超える場合は折りたたみ**、"Show all (N lines)" ボタンで展開
- リンク：アクセント色、hover で下線
- 画像：中 radius、最大幅 100%

### 4.7 Streaming Indicator

LLM 応答がストリーミング中のメッセージ（ドラフト状態）：

- 末尾に点滅カーソル（ブロック形状、800ms の blink）
- bubble 右下に小 spinner または streaming 用ステータスアイコン
- ストリーミング完了（done 受信）後、カーソルを消して確定状態へ移行

---

## 5. Tool 実行カード

LLM がツールを呼び出したとき、応答メッセージの下（同一の assistant 発言ツリー内）に折りたたみカードを表示する。

### 5.1 States

| 状態 | 表示 |
|---|---|
| `pending`（tool_start 受信、tool_result 未受信） | summary に `"running…"`、右端に小 spinner |
| `success` | summary に入力の主要スカラー値、右端に所要時間 badge（`{durationMs}ms`） |
| `error` | summary にエラーメッセージの先頭40字、右端に `"error"` badge |

### 5.2 Summary の生成ルール

カードの closed 状態で表示する1行サマリ。原則「ツール名 + 主要引数の短縮形」。

ツール毎に「どの引数を summary に出すか」のルールを定義する。例：

- ファイル操作系（`read` / `write` / `edit` 等）：対象 path
- `shell`：コマンド文字列
- 検索系（`web_search` 等）：クエリ文字列
- `agent_send`：宛先 agent_id（`→ {to}` 形式）
- その他：入力 JSON のうち最初の string/number 値、もしくは `"…"`

### 5.3 展開時の入出力表示

- input・output を2つの code block で表示
- input は JSON を pretty-print
- output は content-type により言語を切り替え（テキスト・JSON・Markdown 等）
- output が長大な場合（目安として5KB超）、最初の数KBのみ表示し "Show full output" ボタンで段階的に展開

### 5.4 折りたたみのデフォルト

- `tool_start`：閉じた状態で挿入
- `tool_result` 受信：閉じたまま更新
- エラーの場合は自動展開の候補とするが、**1ターン中に複数のエラーツールがある場合は最新1件のみ自動展開**し、それ以前のエラーは閉じた状態を維持（badge で error であることは示す）
- ユーザーが手動で開閉した状態は同一セッション表示中は維持

---

## 6. Composer

### 6.1 構造

- 上部：CommandSuggest（`/` で始まる入力時のみ表示）
- 中央：textarea（最大6行まで自動拡張）
- 下部：ヒント（`Enter` で送信・`Shift+Enter` で改行）+ Send button

### 6.2 キー操作

| キー | 動作 |
|---|---|
| `Enter` | 送信（空文字は送信しない） |
| `Shift+Enter` | 改行挿入 |
| サジェスト表示中 `↑` `↓` | サジェスト選択移動 |
| サジェスト表示中 `Tab` `Enter` | サジェスト確定 |
| サジェスト表示中 `Esc` | サジェスト閉じる |

`Enter=送信 / Shift+Enter=改行` で固定する（Slack・ChatGPT 等の主流パターン）。`Cmd/Ctrl+Enter` を代替送信に割り当てない（重複のため）。

### 6.3 Send Button の状態

| 状態 | label | disabled |
|---|---|---|
| 入力空 | `Send` | true |
| 入力あり・待機中 | `Send` | false |
| 送信中 | 小 spinner + `Sending…` | true |
| read-only | (button 無し) | — |

### 6.4 Placeholder

- 空状態：`"Type a message. / for commands."`
- 送信中：`"Waiting for response…"`（入力不可）

### 6.5 Draft 永続化

入力途中のメッセージを localStorage に保存し、リロードやクラッシュから復元する。

- 保存キー: `egopulse.draft.{sessionKey}`
- 保存タイミング: 入力変更の debounce 300ms後
- 復元タイミング: Composer のマウント時
- 送信成功時: 当該キーを localStorage から削除
- セッション切替時: 切替前に現セッションの draft を保存、切替後に新セッションの draft を復元
- localStorage 利用不可（プライベートモード等）の場合は機能無効化（例外を握り潰す）

### 6.6 CommandSuggest

`/` で始まる入力でサジェストを表示。スラッシュコマンド仕様は [commands.md](../commands.md) 参照。

---

## 7. Read-only Mode

### 7.1 トリガー

選択中セッションの `channel` が `web` 以外のとき、Chat Tab を read-only モードで表示する。

### 7.2 表示

Composer 領域を read-only バナーで差し替える：

```
┌─ Read-only Banner ─────────────────────────────┐
│ 🔒 This is a Discord session.                   │
│   To reply, use Discord directly.               │
└────────────────────────────────────────────────┘
```

- panel-2 背景、枠線あり、特大 radius
- LockIcon（24×24, muted）+ タイトル + 説明
- channel 毎の表示名：

| channel | 表示名 |
|---|---|
| `discord` | `Discord` |
| `telegram` | `Telegram` |
| `cli` | `the CLI` |
| `tui` | `the TUI` |
| `voice` | `a voice device` |

### 7.3 履歴取得

read-only セッションでも `GET /api/history?session_key=chat:{id}` で履歴を取得する。チャネルによる差異は無く、保存されているメッセージをそのまま表示する。

### 7.4 リアルタイム更新

他チャネルで進行中の会話をリアルタイムで反映するため、WS 上の `chat` イベントペイロードに **sessionKey（または chat_id）を含める**。現状では runId しか含まれていないため、この拡張をバックエンドに施す。

UI 側は、選択中の read-only セッションの sessionKey と一致する `chat` イベントを受信した場合、タイムラインへ反映する。これにより、Discord 等で進行中の会話を WebUI を開いたまま追跡できる。

---

## 8. 状態遷移

### 8.1 メッセージライフサイクル

1. ユーザーが入力・Enter 押下
2. ユーザーメッセージを in-memory に楽観追加
3. WS `chat.send` を送信、run_id を受領
4. WS 上でトークン刻みの delta を受信 → ドラフトメッセージへ追記
5. WS 上で done を受信 → ドラフトを確定（message id を確定値へ差し替え）
6. セッション一覧と履歴を refetch、in-memory リストと差し替え

### 8.2 エラー時

- WS 接続切断：トースト `"Connection lost. Retrying…"`、再接続後に再送信を促す
- チャットエラーイベント受信：timeline に system bubble でエラー表示、Composer を再有効化
- HTTP 401：AuthModal 表示、現在のドラフトは保持

---

## 9. アクセシビリティ

- Timeline：`aria-live="polite"`、`aria-label="Conversation history"`
- 各 bubble：`role="article"`、`aria-label="{sender} at {time}"`
- Streaming cursor：`aria-hidden="true"`
- Tool card header：button 相当、`aria-expanded` `aria-controls` 必須
- Tool card body：`role="region"`、`aria-labelledby` で header と紐付け
- Composer textarea：`aria-label="Message input"`、hint を `aria-describedby` で参照
- Send button：送信中 `aria-busy="true"`
- Read-only banner：`role="status"`
