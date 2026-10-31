# WebUI 画面構成の刷新と Agent 画面

[docs/webui/overview.md](../webui/overview.md) の画面構成（agent 画面 Chat / Agent / Schedule、グローバル画面 Metrics / Config）へ WebUI を移行し、Agent 画面を閲覧機能として実装する。

## 1. スコープ

### 1.1 対象

- 画面構成・ナビゲーション・URL の刷新（Sleep タブ → Schedule、Pulse タブ廃止、Metrics / Config をフッターへ）
- Sidebar の Context Section（画面ごとに中段を切替）
- Schedule 画面の暫定形：既存の Sleep run 一覧・詳細を Schedule 配下へ移設（Sidebar 選択 agent に従う）
- Agent 画面（閲覧のみ）：Overview / Soul / Rules / Secret / Memory（Current・History）/ Skills
- 上記のためのバックエンド API と mock
- 関連ドキュメント（api.md / db.md / webui 設計書の API 記述）

### 1.2 対象外

- Agent 画面の編集機能（ファイル・記憶の PUT、手動編集の snapshot 記録）
- Overview の Autonomy カード（Schedule の API と同時に実装）
- Schedule の Upcoming / Pulse / Intentions / Sleep 判定記録
- Metrics / Config 画面の中身（フッターに disabled で配置）

## 2. 決定事項

| 論点 | 決定 | 理由 |
|---|---|---|
| Skills API | agent 配下ではなく `GET /api/skills` | スキルはグローバルで agent 非依存。agent 配下に置くと誤解を生む |
| Model カードの行 | 「既定（profile のないチャネル）」1行 + profile を持つチャネルごとに1行 | profile がなければ全チャネルで解決結果が同一のため |
| モデル解決元の算出 | `resolve_llm_for_agent_channel` を、解決元を併せて返す関数に委譲する形へ分割 | 解決チェーンを二重実装しない |
| prompt ファイルの表現 | `layers: [{scope, path, content, applied}]` に統一（soul / rules / secret） | SOUL のフォールバックと AGENTS の累積を同じ形で表せ、UI も1コンポーネントで済む |
| Secret の本文 | profile は有無のみ返し、本文は `files/secret` を Reveal 時にだけ取得 | 画面表示だけで秘密モード指示を出さない |
| Memory History | `memory_snapshots` のうち、所属 run が `success` / `partial_failure` かつ `content_before != content_after` の行を新しい順に返す | 公開は finalize より前に行われ、この2状態へは公開成功後の finalize でしか遷移しない。`running`（公開待ち・公開失敗）や復旧で `failed` になった run の候補を混入させない。未変更の補完 snapshot はノイズ |
| Schedule の agent フィルタ | 廃止し Sidebar 選択 agent に従う | 設計（schedule.md §1） |
| DiffViewer | `shared/ui` へ移動 | Agent と Schedule の両方で使う |
| 旧 URL（`/agents/:id/sleep/...`） | 互換を持たない | 規約（後方互換は負債） |

## 3. 変更一覧

### 3.1 バックエンド

| 対象 | 変更 |
|---|---|
| `src/config/resolve.rs` | `resolve_llm_origin_for_agent_channel`（ResolvedLlmConfig + provider / model の解決元）を新設し、既存関数はこれに委譲 |
| `src/agent_loop/prompt/sources.rs` | `SoulAgentsLoader::prompt_file_layers(agent_id, kind)` を新設（scope・path・content・applied）。既存の読み込み規則（trim・safe id）を共有 |
| `src/storage/sleep.rs` | `list_memory_changes(agent_id, file, limit)` を新設 |
| `src/channels/web/agent_profile.rs`（新規） | `GET /api/agents/{id}/profile`、`GET /api/agents/{id}/files/{kind}`、`GET /api/agents/{id}/memory/{layer}/history`、`GET /api/skills`、`GET /api/skills/{name}` |
| `src/channels/web/mod.rs` | ルート登録（認証必須の api_routes） |

### 3.2 フロントエンド

| 対象 | 変更 |
|---|---|
| `app/navigation.ts` | `TabId = chat \| agent \| schedule \| metrics \| config`。agent 画面とグローバル画面を区別 |
| `app/router.ts` | 画面ごとの判別共用体で再設計（Agent の section / layer / view、Schedule の runId） |
| `app/WebUI.tsx` | 新ルートに合わせた状態・遷移、Agent / Schedule の描画 |
| `app/shell/Sidebar.tsx` / `AppShell.tsx` | Nav 3タブ、Footer に Metrics / Config、Context Section の切替 |
| `app/shell/AgentSectionNav.tsx`（新規） | Agent 画面のセクション一覧 |
| `app/command-palette/*` | Navigation 項目の更新、Sleep run 遷移先の更新 |
| `features/sleep` → `features/schedule` | `SchedulePanel` に改名し Memory ビューと agent フィルタを除去 |
| `features/agent/*`（新規） | Overview / PromptFileView（soul・rules・secret）/ MemorySection / SkillsSection |
| `shared/ui/DiffViewer.tsx` | 移動 |
| `shared/api/agent.ts`（新規） | 新 API クライアントと型 |
| `web/mock/plugin.ts` | 新 API の mock |

### 3.3 ドキュメント

- `docs/api.md`：新 API
- `docs/db.md`：`list_memory_changes`（存在しない `get_snapshots_for_agent` の記述を置換）
- `docs/webui/agent.md`：API 表を実装に合わせる（skills のパス、Autonomy は Schedule API 依存と明記）

## 4. 実装手順（TDD）

各ステップは失敗するテスト → 実装 → リファクタの順で進め、ステップごとにコミットする。

1. **モデル解決元**：`resolve_llm_origin_for_agent_channel` のテスト（profile / agent / default / provider default の各解決元）
2. **prompt ファイル層**：`prompt_file_layers` のテスト（SOUL のフォールバックと applied、AGENTS の2層、SECRET、unsafe id）
3. **記憶の変更履歴**：`list_memory_changes` のテスト（未変更除外・`running` / `failed` run の候補を除外・`partial_failure` は含む・順序・agent / file 絞り込み・limit）
4. **Web API**：各ハンドラのテスト（正常系、未知 agent 404、未知 kind / layer 400、skills 一覧と単体 404）
5. **ルーティング**：router のテスト（全 URL の parse / build 往復、不正 URL の拒否）
6. **シェル**：Sidebar / AppShell のテスト（Nav 3タブ、Footer の Metrics / Config、Context Section の切替）
7. **Schedule 移設**：既存 Sleep テストの移設・agent フィルタ除去
8. **Agent 画面**：各セクションのテスト（Overview の表示、layers の applied 表示、Secret は Reveal までフェッチしない、Memory History の diff、Skills 選択）
9. **パレット**：Navigation 項目のテスト更新
10. **mock・docs**

## 5. 検証

- `cargo fmt --check` / `cargo clippy --all-targets --all-features -- -D warnings` / `cargo test`
- `npm run typecheck --prefix web` / `npm test --prefix web` / `npm run build --prefix web`
- mock サーバー（`npm run dev:mock`）で Playwright による画面確認：Nav / Footer / Context Section の切替、Agent 各セクション、Secret の Reveal、Memory History の diff、Schedule の Sleep run 詳細、URL の直接アクセスとブラウザバック、モバイル幅
- 自己レビュー後に PR 作成
