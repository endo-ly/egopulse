# EgoPulse WebUI — Chat Tab

Chat タブは、選択中 agent との Web セッションでの対話、および他チャネル（discord / telegram / cli / tui / voice）セッションの read-only 監査を行うメインの対話画面。

## 1. レイアウト

```
┌─ Chat Tab ──────────────────────────────────────────┐
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

Chat Tab は2領域で構成される：

| 領域 | 役割 | 高さ |
|---|---|---|
| Timeline | メッセージ履歴・ストリーミング応答 | 1fr（スクロール） |
| Composer | 入力欄・送信 | auto |

ヘッダー行は持たない。セッション情報は Sidebar の SESSIONS 選択表示に集約され、チャットは Composer を除く縦領域をすべて使う。

Composer は幅 640px の中央カラムに収め、Timeline 上にオーバーレイ表示するフローティングカード（角丸＋影）として浮かべる。メッセージ列の配置は変えない（全幅のまま）。Timeline 末尾には Composer との重なりを避けるクリアランスを確保する。

---

## 2. Channel と read-only

`web` 以外の channel（discord / telegram / cli / tui / voice）のセッションは read-only 監査ビューになり、Composer 領域をバナーで差し替える：

```
┌─ Read-only Banner ─────────────────────────────┐
│ 🔒 This is a Discord session.                   │
│   To reply, use Discord directly.               │
└────────────────────────────────────────────────┘
```

- panel-2 背景・枠線・特大 radius。LockIcon（24×24, muted）+ タイトル + 説明
- channel の表示名は `Discord` / `Telegram` / `the CLI` / `the TUI` / `a voice device`
- 履歴は web と同じ `GET /api/history?session_key=chat:{id}` で取得し、保存済みメッセージをそのまま表示する（チャネル差異なし）
- メッセージ検索は Command Palette の Messages セクションに統合されている（[command-palette.md §4.5](./command-palette.md#45-messages)）

---

## 3. Timeline

### 3.1 構造とスクロール

- 縦方向のスクロール可能なコンテナ。メッセージ間は一定の gap（中大スペース）
- 自動スクロール：新しいメッセージ追加時、最下部からの距離が画面高の 10% 程度未満（auto-follow 判定）の場合のみ下へ追従
- auto-follow 中でなければ、右下に "Jump to latest" ボタンをフロート表示

### 3.2 空状態・ロード中

ロード・再取得・失敗・空の基本パターンは共通（[layout.md §6](./layout.md#6-フォーカスロードエラー)）。Chat 固有として、新規セッションでメッセージ 0 件のとき Timeline 中央に EmptyState（`Start a conversation with {agentName}`）を表示する。

### 3.3 メッセージ検索

Command Palette の Messages セクションから行う（[command-palette.md §4.5](./command-palette.md#45-messages)）。

---

## 4. MessageBubble

### 4.1 Sender 別の配置とスタイル

| `sender_kind` | 配置 | 背景 | 備考 |
|---|---|---|---|
| `user` | 右寄せ | 濃シアンソリッド | ユーザー入力 |
| `assistant` | 中央 | なし（バブルなし・フラット表示） | LLM 応答・Pulse 通知 |
| `system` | 中央（幅 60% 程度） | `panel-2` | システムメッセージ |

ツール実行結果は `message_kind: "tool_call"` で判定し、assistant と同じ中央配置で、内側のカード幅のみ短く表示する。左端は assistant と揃える。

- bubble の最大幅：`min(900px, 100%)`
- user / system の bubble は panel 相当の radius・shadow。assistant は padding・背景・radius を持たない

### 4.2 Avatar とターンヘッダー

Avatar は 40×40 の円形アイコン（`user` は表示しない）。

- `assistant`：agent アイコン。設定済みなら `/api/agents/{agent_id}/avatar` の画像（Bearer 認証付きで取得）、未設定なら sender_id の頭文字
- `system` / `tool`：頭文字

ターンヘッダー（agent の avatar + sender label）は、assistant のターンに属する行のうち最初の行にだけ表示する。

- ナレーション済みの assistant 行が直前にある場合、続くツールカードはヘッダーを繰り返さない
- ターンがツールから始まる場合も、最初のツールカードがヘッダーを持つ

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

Pulse によって Home Surface へ送信されたメッセージは、通常の assistant メッセージとして保存される（[pulse.md §9](../pulse.md#9-出力保存仕様)）。Pulse 由来のメッセージには `message_kind: "pulse_notification"` が付くので、UI はこれで識別する。

識別した Pulse メッセージは以下のように表示する：

- 通常 assistant bubble と同じレイアウト
- bubble の meta 領域に小アイコン + `"Pulse"` ラベル
- アイコン背景：アクセント色の半透明 pill
- tooltip に intention_id と started_at を表示
- メッセージ本文：通常通り Markdown レンダリング

### 4.6 Content Rendering

Markdown をレンダリングする。

- code (inline)：等幅フォント・小サイズ・半透明黒背景・小 radius
- code block：[design-system.md §9.9](./design-system.md#99-code-block) の仕様。Copy ボタン必須、**20行超は折りたたみ**で "Show all (N lines)" ボタンで展開
- リンク：アクセント色、hover で下線
- 画像：中 radius、最大幅 100%

### 4.7 Streaming Indicator

LLM 応答がストリーミング中のメッセージ（ドラフト状態）：

- 末尾に点滅カーソル（ブロック形状、800ms の blink）
- bubble 右下に小 spinner または streaming 用ステータスアイコン
- ストリーミング完了（done 受信）後、カーソルを消して確定状態へ移行

### 4.8 応答待機

run が受諾されてから最初の delta または `tool_start` が届くまでの間、timeline 末尾に thinking dots を持つ assistant の行を表示する（avatar と sender label はターンで最初の行だけ、§4.2）。応答が始まると最初の delta 行またはツールカード行に置き換わる。

---

## 5. Tool 実行カード

LLM がツールを呼び出したとき、応答メッセージの下（同一の assistant 発言ツリー内）に折りたたみカードを表示する。カードは assistant 行と同じ幅で左揃えにし、avatar と sender label はターンで最初の行だけ付く（§4.2）。

### 5.1 States

| 状態 | 表示 |
|---|---|
| `pending`（tool_start 受信、tool_result 未受信） | summary に `"running…"`、右端に小 spinner |
| `success` | summary に入力の主要スカラー値、右端に所要時間 badge（`{durationMs}ms`） |
| `error` | summary にエラーメッセージの先頭40字、右端に `"error"` badge |

### 5.2 Summary の生成ルール

closed 状態で表示する1行サマリ（ツール名 + 主要引数の短縮形）。出す引数はツール毎に：

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

`Enter=送信 / Shift+Enter=改行` で固定する。`Cmd/Ctrl+Enter` を代替送信に割り当てない（重複のため）。

### 6.3 Send Button の状態

円形（32px）の上矢印アイコンボタン。

| 状態 | disabled |
|---|---|
| 入力空 | true |
| 入力あり・待機中 | false |
| 送信中 | true |
| read-only | (button 無し) |

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

---

## 7. 状態遷移

### 7.1 メッセージライフサイクル

1. ユーザーが入力して Enter を押下する
2. ユーザーメッセージを canonical ID（`web:{uuid}`）で in-memory に楽観追加する。この ID は送信・Turn の `request_key`・永続化後の `messages.id` と同一
3. WS `chat.send`（`messageId` 必須）を送信し、受諾 ack（`res` ok）を待つ。WebSocket の RPC ID は attempt ごとに発行し、canonical ID は同じ draft の ACK 不明時の明示的な retry でのみ再利用する
   - 受諾で Composer をクリアし、run ownership を楽観行へ付与して assistant の進捗表示（typing indicator）を出す。進捗は run 単位で管理し、複数 run の同時進行や非 terminal done 後の次 Turn 待機も表現する
   - 拒否・タイムアウト（15 秒）では文面を保持したままエラーを表示する。draft を編集した場合やセッションを切り替えた場合は新しい ID を使う
4. WS 上でトークン刻みの delta を受信したら、その安定 ID（`turn:{turnId}:assistant:{iteration}`）のメッセージへ追記する。Tool Call の前後で ID は変わらず、Tool Card は `callId` で別行に表示する。ナレーションなしの Tool Call では空バブルを作らず Tool Card だけ表示する
5. WS 上で done を受信したら、同じ安定 ID へ確定内容で置換する（authoritative）。履歴とは ID 一致でのみ突き合わせ、内容比較はしない
6. Tool 実行中に送った follow-up は `user_input` の `messageId`（送信時と同一 ID）で upsert する。昇格した子 Turn の初期イベントも同じ ID のまま
7. セッション一覧と履歴を再取得し、表示は履歴と live のマージにする（ID 一致は履歴優先、履歴にない live のみ保持）
8. WS が意図せず切断 → 再接続した場合は未完了 run を `run.subscribe` で再購読し、replay と live に追従する。`replayTruncated` の場合や run 自体が消えていた場合（`run_not_found`）は、その run 由来の live entries を run ownership で破棄し、履歴から再構築する
9. retry で置換された stream 済み応答は `assistant_discarded` で該当 ID を削除し、retry は新しい ID で stream する

### 7.2 リアルタイム更新

セッション一覧と履歴は、WS の `session_updated` で追従する（ペイロードは `chatId` / `sessionKey` のみ）。

- イベント受信時：`sessions` と対象セッションの `history` キャッシュを無効化する。表示中のセッションは即時再取得し、それ以外は次に開いたときに取得する。短時間に連続するイベントはデバウンスして 1 回にまとめる
- `resync: true` 受信時（バッファ超過などでイベントを取りこぼした場合）：セッション一覧とすべての履歴キャッシュを無効化する
- イベント未達時のフォールバックとして、60 秒間隔のポーリングを残す

### 7.3 エラー時

- WS 接続切断：トースト `"Connection lost. Retrying…"`、再接続後に再送信を促す
- チャットエラーイベント受信：timeline に system bubble でエラー表示、Composer を再有効化
- HTTP 401：AuthModal 表示、現在のドラフトは保持

---

## 8. アクセシビリティ

- Timeline：`aria-live="polite"`、`aria-label="Conversation history"`
- 各 bubble：`role="article"`、`aria-label="{sender} at {time}"`
- Streaming cursor：`aria-hidden="true"`
- Tool card header：button 相当、`aria-expanded` `aria-controls` 必須
- Tool card body：`role="region"`、`aria-labelledby` で header と紐付け
- Composer textarea：`aria-label="Message input"`、hint を `aria-describedby` で参照
- Send button：送信中 `aria-busy="true"`
- Read-only banner：`role="status"`
