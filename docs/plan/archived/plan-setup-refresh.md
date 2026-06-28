# Plan: Setup Wizard Refresh

`egopulse setup` を ratatui フル TUI から `dialoguer` ベースのチャットライク順次プロンプトへ全面刷新する。設計元: `docs/setup-redesign.md`。

> **Note**: 振る舞い (What) は決して変えてはいけないが、より美しい設計があれば実装方法 (HOW) だけは変えてもよい。

## 設計方針

- **Agent-First**: 最初の質問は Agent Label。続く Provider/Model は「その Agent が使う LLM」として位置づける (`docs/setup-redesign.md §2.1`)
- **Minimum Viable Setup**: LLM と対話するための最低限のみ問い、詳細項目はデフォルト運用
- **既存資産の流用**: `PROVIDER_PRESETS` / `build_channel_configs` / `generate_auth_token` / `validate_fields` / `save_config` / `backup_config` は残置し、新フローに合わせて拡張する
- **チャットライクプロンプトには `dialoguer` を採用** (AGENTS.md「既存ライブラリ優先」)
- **インタラクションロジックを pure function に切り出し**、dialoguer に依存しない部分をユニットテスト可能にする
- **既存の `ratatui` / `crossterm` 依存は残置** (TUI チャネル `src/channels/tui.rs` が残るため。依存削除は別フェーズ)
- **`run_setup_wizard()` のシグネチャは維持**し、`src/main.rs:97-101` の呼び出し側は変更しない
- 参照: `docs/setup-redesign.md` (設計メモ)、`docs/commands.md §1.1`、`docs/config.md §7`、`docs/channels.md`

## TDD 方針

テストリスト項目 (T1, T2...) と自動テスト (`test_name`) を明確に区別する。1回の Red では自動テストを 1 件だけ追加し、Green・Refactor を混ぜない。1つのテストリスト項目に複数の境界・異常系がある場合は、同じ項目を対象にした Cycle を複数作る。実装中に新たな不安を見つけたらテストリストへ追加し、次の Cycle で扱う。Red→Green→Refactor が終わったら即コミット。

## Plan スコープ

WT作成 → 実装(TDD) → コミット(意味ごとに分離) → PR作成

## 対象一覧

| 対象 | 種別 | 既存パターン / 参照元 | 備考 |
| -- | -- | -- | -- |
| `src/setup/mod.rs` | 大規模削除 + 一部残置 | 既存 TUI 実装 (`SetupApp`, `init_terminal`, `draw_*`, `handle_*_key`) | 約 900 行削除。`run_setup_wizard()` の再エクスポートのみ残す |
| `src/setup/prompts.rs` | **新規** | なし | `dialoguer` ラッパー。純粋関数 (validation, format) を分離してテスト可能に |
| `src/setup/wizard.rs` | **新規** | なし | Welcome→Q1〜Q7→Review→Save→Additional Options→Done のフロー制御。状態遷移は pure に |
| `src/setup/provider.rs` | 一部削除 + 残置 | `PROVIDER_PRESETS`, `find_provider_preset`, `normalize_provider_id`, `provider_label_for` を残置 | `SelectorItem` / `SelectorState` / `enter_selector` 等 TUI 依存部は削除 |
| `src/setup/channels.rs` | 拡張 + 残置 | `build_channel_configs`, `generate_auth_token`, `extract_existing_state_root` | `build_channel_configs` に `web_enabled: bool` パラメータを追加 (S2 是正) |
| `src/setup/summary.rs` | 拡張 + 残置 + 一部削除 | `validate_fields`, `save_config`, `backup_config`, `cleanup_old_backups`, `mask_secret` を残置 | `draw_completion_summary` は ratatui 依存のため削除。`Field` 構造体廃止に伴い `save_config` シグネチャを新データ型へ |
| `Cargo.toml` | 変更 | 既存依存 | `dialoguer`, `url` を追加 (`url` が未対応の場合) |
| `docs/setup-redesign.md` | 変更 | 本メモ | Status を「実装済み」へ更新 |
| `docs/commands.md §1.1` | 変更 | `egopulse setup` 行 | 「対話型設定プロンプト」へ |
| `docs/config.md §7` | 全面書き換え | 「セットアップウィザード」節 | 新仕様 (チャットライクフロー) へ |
| `README.md` | 変更 | Getting Started | `egopulse setup` の説明整合性確認 |
| `src/main.rs` | 変更不要 | 既存呼び出し `setup::run_setup_wizard(cli.config.clone())` | シグネチャ互換を維持 |

## テストリスト / 不安リスト

| ID | 観点 | 期待する振る舞い | 優先 | 対応するCycle | 状態 / 今回対象外理由 |
| -- | -- | -- | -- | -- | -- |
| T1 | 正常系 | `slugify("Lyre")` → `"lyre"` | High | Step 1 | 未着手 |
| T2 | 正常系 | `slugify("My Agent")` → `"my-agent"` | High | Step 1 | 未着手 |
| T3 | 正常系 | `slugify("Vega 2")` → `"vega-2"` (英数字混在) | High | Step 1 | 未着手 |
| T4 | 境界値 | `slugify("  Multi   Space  ")` → `"multi-space"` (連続非英数圧縮 + 前後ハイフン削除) | High | Step 1 | 未着手 |
| T5 | 空・ゼロ状態 | `slugify("")` / `slugify("!!!")` / `slugify("   ")` → `"default"` フォールバック | High | Step 1 | 未着手 |
| T6 | エッジケース | `slugify("日本語Agent")` → `"agent"` (非 ASCII はハイフン扱い→圧縮) | Medium | Step 1 | 未着手 |
| T7 | 異常系 | `validate_fields` が provider 空を `Err` | High | Step 2 | 未着手 |
| T8 | 異常系 | `validate_fields` が不正 base_url を `Err` | High | Step 2 | 未着手 |
| T9 | 異常系 | `validate_fields` が Discord 有効 + トークン空を `Err` | High | Step 2 | 未着手 |
| T10 | 正常系 | `validate_fields` が最低限セット (provider, base_url, model, api_key 省略/localhost) を許可 | High | Step 2 | 未着手 |
| T11 | 境界値 | `validate_fields` が API key 空 + localhost 系 base_url を許可 | Medium | Step 2 | 未着手 |
| T12 | 正常系 | `build_channel_configs(web_enabled=true)` が `web` エントリを含む | High | Step 3 | 未着手 |
| T13 | 空・ゼロ状態 | `build_channel_configs(web_enabled=false)` が `web` エントリを含まない (S2 是正) | High | Step 3 | 未着手 |
| T14 | 正常系 | `build_channel_configs` が Discord/Telegram 有効時に各エントリを生成 | Medium | Step 3 | 未着手 |
| T15 | 正常系 | `save_config` が agent label を `agents.<slugified-id>.label` に保存 | High | Step 4 | 未着手 |
| T16 | 正常系 | `save_config` が `default_agent` を ユーザー入力 id に設定 | High | Step 4 | 未着手 |
| T17 | 空・ゼロ状態 | `save_config` が web 無効化時に `channels.web` エントリを**保存しない** (Discord/Telegram と一貫、`enabled:false` 残しではない) | High | Step 4 | 未着手 |
| T18 | 異常系 | `save_config` が既存ファイル存在時に backup を生成 | Medium | Step 4 | 未着手 |
| T19 | 統合 | `save_config` → 再 `Config::load` でラウンドトリップ可能 | High | Step 4 | 未着手 |
| T20 | 異常系 | `save_config` が既存 `WEB_AUTH_TOKEN` を**再利用** (新規生成しない) | High | Step 4 | 未着手 |
| T21 | 正常系 | `save_config` が既存 `state_root` を**保持** (上書きしない) | High | Step 4 | 未着手 |
| T22 | 異常系 | `parse_existing_config` が YAML パースエラー時にエラー情報を保持 (呼び出し側で warn 表示) | Medium | Step 5 | 未着手 |
| T23 | 正常系 | `parse_existing_config` (純粋関数に切り出し) が正常 YAML をフィルドマップへ変換 | Medium | Step 5 | 未着手 |
| T24 | 正常系 | `mask_secret` が短い (≤8 文字) を `********` にマスク (既存挙動維持) | Low | Step 5 | 未着手 |
| T25 | 正常系 | `format_api_key_for_review(api_key)` が `sk-...xxxx` 形式 (末尾4文字) を返す | Medium | Step 6 | 未着手 |
| T26 | 空・ゼロ状態 | `format_api_key_for_review("")` が `"(empty)"` を返す | Medium | Step 6 | 未着手 |
| T27 | 正常系 | `review_decision_from_index(0/1/2)` が `StartOver/Abort/SaveAnyway` を返す | High | Step 7 | 未着手 |
| T28 | 正常系 | `build_review_summary(inputs)` が期待するテキストブロックを生成 | Medium | Step 7 | 未着手 |
| T29 | 正常系 | `build_additional_options_text()` が `docs/setup-redesign.md §4.2` の構成で生成 | Medium | Step 7 | 未着手 |
| T30 | 正常系 | `build_done_message(inputs)` が保存先・次ステップ・Web/Discord/Telegram 案内を含む | Medium | Step 7 | 未着手 |
| T31 | 境界値 | `should_confirm_empty_api_key(provider, base_url)` が **localhost 系では false** を返す (スキップ) | High | Step 7 | 未着手 |
| T32 | 境界値 | `should_confirm_empty_api_key(provider, base_url)` が **非 localhost で true** を返す | High | Step 7 | 未着手 |
| T33 | 正常系 | `is_custom_provider(provider_id)` が `custom` で true、他は false を返す | Medium | Step 7 | 未着手 |
| T34 | 正常系 | `should_ask_model_as_free_text(provider_id)` が Custom で true、preset で false を返す | Medium | Step 7 | 未着手 |
| T35 | 正常系 | 既存 `default_provider` / `agents.<id>.label` 等が各 prompt の default に事前入力される (`docs/setup-redesign.md §5.3`) | High | Step 8 | 未着手 |
| T36 | 統合 | wizard Review で no → `StartOver` 選択で Q1 に戻りループする | High | Step 8 | 未着手 |
| T37 | 異常系 | wizard Review で no → `Abort` 選択でファイル未保存のまま終了する | High | Step 8 | 未着手 |
| T38 | 正常系 | wizard Review で no → `SaveAnyway` 選択で保存して Done へ進む | High | Step 8 | 未着手 |
| T39 | 正常系 | wizard Review で `yes` 直接選択で保存して Done へ進む | High | Step 8 | 未着手 |
| T40 | 異常系 | wizard 既存 YAML パースエラー時、ユーザーが N 選択で中断する | High | Step 8 | 未着手 |
| T41 | 正常系 | wizard 既存 YAML パースエラー時、ユーザーが Y 選択で空状態から継続する | High | Step 8 | 未着手 |

---

## Step 0: Worktree 作成

- ブランチ名: `feat/setup-wizard-refresh`
- 作成コマンド:
  - `git worktree add ./wt-setup-refresh -b feat/setup-wizard-refresh origin/main`
- 作成後、`docs/setup-redesign.md` が最新であることを確認

---

## Step 1: slugify TDD Cycle - Agent Label → agent id 変換

### この Step の目的

Agent Label から agent id を生成する純粋関数 `slugify_agent_id(label: &str) -> String` を実装する。

### 今回選ぶ項目

- 対象: `T1`, `T2`, `T3`, `T4`, `T5`, `T6`
- 選ぶ理由: 後続の `save_config` 拡張 (Step 4) に必要な基盤。入出力が明確で TDD に適し、設計判断の余地が小さい
- この時点では扱わないこと: `save_config` への統合、フロー全体

### RED: 失敗する自動テストを書く

- 追加するテスト名 (複数 Cycle に分割):
  - `slugify_lowercases_ascii_letters` (T1)
  - `slugify_replaces_whitespace_with_hyphen` (T2)
  - `slugify_preserves_alphanumeric` (T3)
  - `slugify_compresses_consecutive_separators_and_trims` (T4)
  - `slugify_falls_back_to_default_for_empty_or_symbols_only` (T5)
  - `slugify_replaces_non_ascii_with_hyphen` (T6)
- Given: 入力文字列
- When: `slugify_agent_id(input)` を呼ぶ
- Then: 期待の agent id 文字列
- 失敗理由の想定: 関数が未実装のためコンパイルエラー

### GREEN: 最小実装

`src/setup/mod.rs` または新設の `src/setup/slugify.rs` に private 関数として実装。方針:

1. lowercase 化
2. 文字ごとに: ASCII 英数字 → そのまま、それ以外 → ハイフン
3. 連続ハイフンを 1 つに圧縮
4. 前後のハイフンを削除
5. 結果が空なら `"default"` を返す

### REFACTOR: 設計の整理

- 重複: 同様の正規化処理が既存コード (`normalize_provider_id`) にないか確認。目的が違うなら混ぜない
- 命名: `slugify_agent_id` とし、`slugify` 単独より目的を明示
- 責務: 入力→出力の純粋関数。副作用なし
- テストの構造的結合: private fn でも `#[cfg(test)] mod tests` から `use super::*` で呼べる
- 次の項目へ進める身軽さ: `save_config` 側から呼べる状態

### テストリスト更新

- 完了: `T1`, `T2`, `T3`, `T4`, `T5`, `T6`
- 追加: なし
- 次候補: `T7` (validate_fields 拡張)

### コミット

`feat(setup): add slugify_agent_id for agent label normalization`

---

## Step 2: validate_fields 拡張 TDD Cycle - 新入力データ型への対応

### この Step の目的

`Field` 構造体廃止を見据え、新しい入力データ型 (例: `SetupInputs` 構造体) に対するバリデーションを実装する。`validate_fields` を置き換える `validate_inputs(inputs: &SetupInputs) -> Result<(), String>` を新設。

### 今回選ぶ項目

- 対象: `T7`, `T8`, `T9`, `T10`, `T11`
- 選ぶ理由: `save_config` 拡張 (Step 4) の前提。異常系を先に固めることで後続 Step の安全性が増す
- この時点では扱わないこと: `save_config`、`build_channel_configs`

### RED: 失敗する自動テストを書く

- 追加するテスト名:
  - `validate_inputs_rejects_empty_provider` (T7)
  - `validate_inputs_rejects_invalid_base_url` (T8)
  - `validate_inputs_rejects_discord_enabled_without_token` (T9)
  - `validate_inputs_accepts_minimum_set` (T10)
  - `validate_inputs_allows_empty_api_key_for_localhost` (T11)
- Given: `SetupInputs` の各種バリエーション
- When: `validate_inputs(&inputs)`
- Then: `Ok(())` または `Err(message)`
- 失敗理由の想定: `SetupInputs` 型および `validate_inputs` 関数が未定義

### GREEN: 最小実装

- `SetupInputs` 構造体を新設 (`src/setup/summary.rs` 内、または新モジュール)。フィールド: `agent_label: String`, `provider_id: String`, `base_url: String`, `model: String`, `api_key: String`, `web_enabled: bool`, `discord_enabled: bool`, `discord_bot_token: String`, `telegram_enabled: bool`, `telegram_bot_token: String`, 必要に応じて `custom_base_url: Option<String>`
- `validate_inputs` を実装。既存 `validate_fields` ロジックを `SetupInputs` 向けに移植
- 既存 `validate_fields` はこの Step では**残置** (Step 9 で削除)

### REFACTOR: 設計の整理

- 重複: `validate_fields` と `validate_inputs` が並存する一時的な状態。Step 9 で解消
- 命名: `validate_inputs` (複数形は避け単数形で)
- 責務: 入力チェックのみ。副作用なし
- テストの構造的結合: 内部の検査順序に依存しない (エラーメッセージで判断しない、`is_err()` と `unwrap_err().contains("...")` で安定)
- 次の項目へ進める身軽さ: Step 3 へ

### テストリスト更新

- 完了: `T7`, `T8`, `T9`, `T10`, `T11`
- 追加: なし
- 次候補: `T12` (build_channel_configs web 無効化対応)

### コミット

`feat(setup): add SetupInputs type and validate_inputs for chat-based wizard`

---

## Step 3: build_channel_configs 拡張 TDD Cycle - Web 強制有効化の廃止

### この Step の目的

`build_channel_configs` に `web_enabled: bool` パラメータを追加し、Web を使わない選択肢を提供する (設計メモ §3.1 / S2 是正)。

### 今回選ぶ項目

- 対象: `T12`, `T13`, `T14`
- 選ぶ理由: 既存の強制有効化ロジックを安全に切り替える。既存テスト (`build_channel_configs_stores_channel_secrets_as_env_refs`) との整合性も保つ
- この時点では扱わないこと: `save_config`、`wizard` フロー

### RED: 失敗する自動テストを書く

- 追加するテスト名:
  - `build_channel_configs_includes_web_when_enabled` (T12)
  - `build_channel_configs_omits_web_when_disabled` (T13)
  - `build_channel_configs_includes_discord_and_telegram_when_enabled` (T14)
- 既存テスト `build_channel_configs_stores_channel_secrets_as_env_refs` は新しいシグネチャに更新
- Given: `web_enabled` の true/false
- When: `build_channel_configs(web_enabled, discord_enabled, telegram_enabled, ...)`
- Then: `channels` マップのキーに `web` が含まれる/含まれない
- 失敗理由の想定: 既存シグネチャ (`auth_token: String, discord_enabled, telegram_enabled, ...`) と不一致

### GREEN: 最小実装

- `build_channel_configs` のシグネチャ変更: `web_enabled: bool` を冒頭に追加
- Web エントリの insert を `if web_enabled { channels.insert(...) }` でガード (**Discord/Telegram と同じパターンで一貫**)
- 無効化時に `channels.web` マップ自体を含めない (`enabled: Some(false)` で残す**ではない**)
- 既存テスト `build_channel_configs_stores_channel_secrets_as_env_refs` は `web_enabled = true` で呼び出すよう修正

### REFACTOR: 設計の整理

- 重複: なし
- 命名: パラメータ順は `web_enabled, auth_token, discord_enabled, telegram_enabled, ...` (頻度順)
- 責務: ChannelConfig 生成のみ。IO なし
- テストの構造的結合: 戻り値の `HashMap` のキー存在で検証、内部構造に踏み込みすぎない
- 次の項目へ進める身軽さ: Step 4 へ

### テストリスト更新

- 完了: `T12`, `T13`, `T14`
- 追加: なし
- 次候補: `T15` (save_config agent label 対応)

### コミット

`feat(setup): allow web channel disablement in build_channel_configs`

---

## Step 4: save_config 拡張 TDD Cycle - 新 SetupInputs 対応 + Agent-First

### この Step の目的

`save_config` を `SetupInputs` 受け取りに変更し、agent label → slugify で id 生成、`default_agent` / `agents.<id>.label` を反映、Web 無効化対応。

### 今回選ぶ項目

- 対象: `T15`, `T16`, `T17`, `T18`, `T19`, `T20`, `T21`
- 選ぶ理由: 永続化ロジックの核心。ラウンドトリップ (T19) で設定ファイル仕様との整合を担保。T20/T21 は既存ユーザーが setup を再実行した際の回帰防止 (WEB_AUTH_TOKEN のローテーション、state_root の上書きを防ぐ)
- この時点では扱わないこと: wizard フロー、Review 表示

### RED: 失敗する自動テストを書く

- 追加するテスト名:
  - `save_config_persists_agent_label` (T15)
  - `save_config_sets_default_agent_to_user_id` (T16)
  - `save_config_omits_web_entry_when_disabled` (T17)
  - `save_config_creates_backup_when_existing_file_present` (T18)
  - `save_config_roundtrips_with_config_load` (T19)
  - `save_config_reuses_existing_web_auth_token` (T20)
  - `save_config_preserves_existing_state_root` (T21)
- 既存 `save_config(fields, original_yaml, config_path)` から `save_config(inputs: &SetupInputs, original_yaml: &Option<...>, config_path: &Path)` へ
- Given: `SetupInputs` + 一時ディレクトリ
- When: `save_config(...)` → `Config::load(path)`
- Then: ロードした `Config` の各フィールドが入力と一致
- 失敗理由の想定: シグネチャ変更で既存呼び出し元が壊れる (Step 9 で `SetupApp::save` も消すため一時的に `#[allow(dead_code)]` は使わず、一時的に旧 `save_config` を残置してもよい)

### GREEN: 最小実装

- 新 `save_config(inputs: &SetupInputs, original_yaml, config_path) -> Result<(Option<String>, Vec<String>), String>` を実装
- 内部で `slugify_agent_id(&inputs.agent_label)` を呼び出し agent id を生成
- `agents` マップをユーザー入力 id で上書き (`{id}: { label: inputs.agent_label, ..default }`)
- `default_agent` をその id に
- `build_channel_configs(inputs.web_enabled, ...)` を呼び出し
- **既存設定の保持** (T20/T21):
  - 既存 `Config::load_allow_missing_api_key` で `web_auth_token()` が取得できれば再利用、なければ `generate_auth_token()` (既存 `save_config` と同じ挙動)
  - `extract_existing_state_root(original_yaml)` で既存 `state_root` を取得し、新 Config に引き継ぎ
- 既存 `save_config(fields, ...)` の呼び出し元 (`SetupApp::save`) は Step 9 で消すので、この Step では新設のみ

### REFACTOR: 設計の整理

- 重複: 旧 `save_config` と新 `save_config` が一時的に並存。Step 9 で旧を削除
- 命名: 新関数は `save_config_from_inputs` 等にして一時的な衝突を避ける案もあり (How は実装者判断)
- 責務: YAML + `.env` 永続化、バックアップ、completion summary 生成
- テストの構造的結合: 一時ディレクトリで検証、実環境に依存しない
- 次の項目へ進める身軽さ: Step 5 へ

### テストリスト更新

- 完了: `T15`, `T16`, `T17`, `T18`, `T19`, `T20`, `T21`
- 追加: なし
- 次候補: `T22` (parse_existing_config)

### コミット

`feat(setup): support agent label, web disablement and existing value preservation in save_config`

---

## Step 5: load_existing_config 改善 TDD Cycle - パースエラーの黙殺廃止

### この Step の目的

既存 `SetupApp::load_existing_config` のパースエラー黙殺を廃止し、エラー情報を呼び出し元で扱えるよう pure 関数 `parse_existing_config(yaml_text) -> Result<ParsedConfig, ParseError>` を切り出す。

### 今回選ぶ項目

- 対象: `T22`, `T23`, `T24`
- 選ぶ理由: 設計メモ §3.1「既存 Config パースエラーの warn 表示 (Y/N 確認付き)」の実現基盤。Step 8 wizard で呼ぶ
- この時点では扱わないこと: Y/N 確認 UI (Step 8 wizard 側で実装)

### RED: 失敗する自動テストを書く

- 追加するテスト名:
  - `parse_existing_config_returns_err_for_invalid_yaml` (T22)
  - `parse_existing_config_extracts_provider_schema` (T23)
  - `mask_secret_fully_masks_short_values` (T24) - ついでに `mask_secret` の回帰テスト
- Given: YAML 文字列 (正常/壊れ)
- When: `parse_existing_config(text)`
- Then: 正常なら `Ok(fields)`、壊れていれば `Err`
- 失敗理由の想定: 関数未実装

### GREEN: 最小実装

- `src/setup/summary.rs` または `src/setup/mod.rs` に `parse_existing_config` を新設
- 既存 `SetupApp::load_existing_config` のパース部分を切り出し
- 戻り値は `Result<ExistingConfig, String>` (既存 `HashMap<String, String>` と `Option<YamlValue>` の組、または新構造体)
- `mask_secret` は既存実装を維持、テスト追加のみ

### REFACTOR: 設計の整理

- 重複: `SetupApp::load_existing_config` と `parse_existing_config` が一時並存。Step 9 で旧を削除
- 命名: `parse_existing_config` (pure 関数)
- 責務: テキスト→構造化。IO なし
- テストの構造的結合: YAML 文字列を直接渡す、ファイル IO に依存しない
- 次の項目へ進める身軽さ: Step 6 へ

### テストリスト更新

- 完了: `T22`, `T23`, `T24`
- 追加: なし
- 次候補: `T25` (format_api_key_for_review)

### コミット

`refactor(setup): extract parse_existing_config as pure function`

---

## Step 6: Review/Done 表示用フォーマット関数 TDD Cycle

### この Step の目的

Review 画面の API Key マスク表示、Done メッセージの構築を pure 関数として実装し、テスト可能にする。

### 今回選ぶ項目

- 対象: `T25`, `T26`
- 選ぶ理由: dialoguer に依存しない UI 構成要素。先に純粋関数で固める
- この時点では扱わないこと: 実際の描画 (Step 8 wizard)

### RED: 失敗する自動テストを書く

- 追加するテスト名:
  - `format_api_key_for_review_masks_long_values` (T25) - `sk-xxxxxxxxxxxx` → `sk-...xxxx` (末尾4文字)
  - `format_api_key_for_review_shows_empty_for_blank` (T26)
- Given: API key 文字列
- When: `format_api_key_for_review(key)`
- Then: 期待のマスク文字列
- 失敗理由の想定: 関数未実装

### GREEN: 最小実装

- `src/setup/prompts.rs` (新設) または `src/setup/summary.rs` に `format_api_key_for_review` を実装
- 既存 `mask_secret` との差分: Review 用は「先頭2文字 + `...` + 末尾4文字」形式 (`docs/setup-redesign.md §4.2 Review`)
- 仕様確認: `docs/setup-redesign.md` では `sk-...xxxx` 記載。「先頭 + `...` + 末尾4文字」。先頭何文字かは実装時に確定 (How は実装者判断)

### REFACTOR: 設計の整理

- 重複: `mask_secret` (完全マスク) と `format_api_key_for_review` (部分マスク) は目的が違う。混ぜない
- 命名: 目的を明示
- 責務: 表示用文字列生成のみ
- テストの構造的結合: 文字列入出力のみ
- 次の項目へ進める身軽さ: Step 7 へ

### テストリスト更新

- 完了: `T25`, `T26`
- 追加: なし
- 次候補: `T27` (review_decision_from_index)

### コミット

`feat(setup): add format_api_key_for_review for Review step`

---

## Step 7: wizard フロー純粋関数 TDD Cycle - 状態遷移・メッセージ構築・分岐判断

### この Step の目的

`wizard.rs` に (a) 状態遷移とメッセージ構築、(b) 分岐判断純粋関数 を実装する。dialoguer に依存しない部分を先に固めることで、Step 8 の統合時に回帰リスクを最小化する。

### 今回選ぶ項目

- 対象: `T27`, `T28`, `T29`, `T30`, `T31`, `T32`, `T33`, `T34`
- 選ぶ理由: インタラクションロジックのコア。Step 8 の dialoguer 統合だけで回帰を防げない指摘 (codex レビュー指摘2) を受けて、分岐判断も純粋関数化してテスト可能にする
- この時点では扱わないこと: dialoguer 呼び出し、IO

### RED: 失敗する自動テストを書く

- 追加するテスト名 (メッセージビルダー系):
  - `review_decision_from_index_maps_correctly` (T27) - 0→StartOver, 1→Abort, 2→SaveAnyway
  - `build_review_summary_renders_all_fields` (T28) - `docs/setup-redesign.md §4.2 Review` 構成
  - `build_additional_options_text_includes_all_categories` (T29) - System/Web UI/Channels/Subsystems
  - `build_done_message_includes_next_steps_and_channel_hints` (T30)
- 追加するテスト名 (分岐判断系):
  - `should_confirm_empty_api_key_returns_false_for_localhost` (T31) - Ollama/LMStudio 等
  - `should_confirm_empty_api_key_returns_true_for_remote` (T32) - OpenAI/OpenRouter 等
  - `is_custom_provider_returns_true_only_for_custom` (T33) - base_url 入力の要否
  - `should_ask_model_as_free_text_returns_true_only_for_custom` (T34) - モデル手入力の要否
- Given: 入力データ (`SetupInputs`, provider_id, base_url 等)
- When: 各ビルド・分岐関数を呼ぶ
- Then: 期待の文字列 (部分一致) または真偽値
- 失敗理由の想定: 関数未実装

### GREEN: 最小実装

- `src/setup/wizard.rs` (新規) を作成
- メッセージビルダー:
  - `ReviewDecision` enum (`StartOver`, `Abort`, `SaveAnyway`)
  - `review_decision_from_index(usize) -> ReviewDecision`
  - `build_review_summary(&SetupInputs) -> String`
  - `build_additional_options_text() -> String` (固定テキスト)
  - `build_done_message(&SetupInputs, config_path, backup_path: Option<String>) -> String`
- 分岐判断 (pure 関数):
  - `should_confirm_empty_api_key(provider_id: &str, base_url: &str) -> bool` - localhost 判定は既存 `codex_auth::provider_allows_empty_api_key` と同じ基準を再利用
  - `is_custom_provider(provider_id: &str) -> bool` - `find_provider_preset` が None を返すかどうか
  - `should_ask_model_as_free_text(provider_id: &str) -> bool` - `is_custom_provider` と同等 (Custom には preset models がないため)

### REFACTOR: 設計の整理

- 重複: メッセージ断片の重複を避ける (例: Web 案内の `http://127.0.0.1:10961` は定数化)
- 命名: `build_*` (文字列生成系) / `should_*` (判断系) / `is_*` (分類系) で揃える
- 責務: 表示文字列生成・分岐判断。副作用なし
- テストの構造的結合: 文字列は `contains`、真偽値は `is_true()`/`is_false()` で検証
- 次の項目へ進める身軽さ: Step 8 (dialoguer 統合) へ。Step 8 はこれら純粋関数を並べて呼ぶだけになるので回帰リスクが下がる

### テストリスト更新

- 完了: `T27`, `T28`, `T29`, `T30`, `T31`, `T32`, `T33`, `T34`
- 追加: なし
- 次候補: なし (純粋関数はここまで)

### コミット

`feat(setup): add wizard message builders, review decision and branch predicates`

---

## Step 8: dialoguer 依存追加と wizard 統合 (trait 抽象 + モック駆動テスト付き)

### この Step の目的

`dialoguer` を Cargo.toml に追加し、`prompts.rs` に dialoguer ラッパーを実装、`wizard.rs` にフロー全体を統合する。**重要**: wizard 本体は `PromptSource` / `OutputSink` trait を介して入出力を抽象化し、dialoguer 実装とモック実装を切り替え可能にする。これにより、Step 7 の predicate 単体では守れない「正しい順序・正しい配線」を、wizard 全体の統合テストで機械的に保証する (codex 2回目レビュー指摘2 対応)。また既存設定値を各プロンプトの default として事前入力する (codex 2回目レビュー指摘1 対応、`docs/setup-redesign.md §5.3`)。

### 今回選ぶ項目

- 対象: `T35`, `T36`, `T37`, `T38`, `T39`, `T40`, `T41`
- 選ぶ理由: リライトで一番壊れやすい wizard 制御 (StartOver/Abort/SaveAnyway/パースエラー時の Y/N) を自動テストで守る。現行実装が持つ「既存設定の事前入力」も回帰させない
- この時点では扱わないこと: 手動確認 (T42)

### RED: 失敗する自動テストを書く

`wizard` 本体は trait 抽象を介して駆動する。モック実装 (`MockPromptSource`, `VecOutputSink`) を使ってフロー全体を検証:

- 追加するテスト名:
  - `prefill_defaults_uses_existing_config_values` (T35) - 既存 `default_provider` や `agents.<id>.label` が各 prompt の default に事前入力される
  - `wizard_review_startover_returns_to_q1` (T36) - Review で no → `StartOver` 選択で Q1 に戻りループ
  - `wizard_review_abort_exits_without_save` (T37) - Review で no → `Abort` 選択でファイル未保存のまま終了
  - `wizard_review_save_anyway_writes_config` (T38) - Review で no → `SaveAnyway` 選択で保存して Done へ
  - `wizard_review_yes_saves_directly` (T39) - Review で `yes` 直接選択で保存して Done へ
  - `wizard_parse_error_decline_aborts` (T40) - 既存 YAML が壊れていてユーザーが N 選択で中断
  - `wizard_parse_error_accept_continues` (T41) - 既存 YAML が壊れていてユーザーが Y 選択で空状態から継続
- Given: モック prompt source (入力シーケンス) + 一時ディレクトリの config_path
- When: `wizard::run_with_source_and_sink(&source, &sink, config_path)`
- Then: モック sink の出力順序、最終的な config_path の有無・内容、戻り値の Ok/Err

### GREEN: 最小実装

- `Cargo.toml` に `dialoguer = "0.x"` を追加 (`cargo add dialoguer` で最新確認)
- `src/setup/prompts.rs` を新規作成:
  - `trait PromptSource` - `fn text(&self, label: &str, default: &str) -> Result<String, ...>`, `fn password(&self, label: &str) -> Result<String, ...>`, `fn select(&self, label: &str, items: &[String]) -> Result<usize, ...>`, `fn confirm(&self, label: &str, default: bool) -> Result<bool, ...>`
  - `trait OutputSink` - `fn print(&self, text: &str)`, `fn println(&self, text: &str)`
  - `DialoguerPromptSource` (本番用実装) と `DialoguerOutputSink`
  - `MockPromptSource` / `VecOutputSink` (`#[cfg(test)]` 内)
- `src/setup/wizard.rs` に `run_with_source_and_sink(source: &dyn PromptSource, sink: &dyn OutputSink, config_path: Option<PathBuf>) -> Result<(), String>` を実装:
  - `parse_existing_config` で既存値を取得 → Q1〜Q7 の各 prompt の default へ事前入力 (T35)
  - 既存 YAML パースエラー時は warn + `source.confirm` で Y/N、N なら即 `Err` (T40)、Y なら空状態で続行 (T41)
  - Review で `source.confirm` が false → `source.select` で3択、`review_decision_from_index` で `StartOver` (T36) / `Abort` (T37) / `SaveAnyway` (T38) へ分岐
  - Review で `source.confirm` が true → 直接 `save_config` 呼び出し (T39)
  - `StartOver` はループで Q1 に戻る、`Abort` は `Err("Setup aborted")` で返す
  - Welcome / Additional Options / Done は `sink.println` で出力
- 公開エントリの `wizard::run(config_path)` は `run_with_source_and_sink(&DialoguerPromptSource::new(), &DialoguerOutputSink::new(), config_path)` を呼ぶ thin wrapper

### REFACTOR: 設計の整理

- 重複: `DialoguerPromptSource` 内の dialoguer 呼び出しの重複を避ける (`select` 共通化など)
- 命名: `prompt_*` (関数) / `PromptSource` (trait) / `DialoguerPromptSource` / `MockPromptSource` で一貫
- 責務: `PromptSource` / `OutputSink` は入出力、`wizard::run_with_source_and_sink` はフロー制御、`summary.rs`/`channels.rs`/`provider.rs` はデータ変換
- テストの構造的結合: モックの入力シーケンスは `Vec<MockInput>` で表現、順序に依存しすぎないよう「ラベル一致」で消費
- 次の項目へ進める身軽さ: Step 9 (旧 TUI 削除) へ

### テストリスト更新

- 完了: `T35`, `T36`, `T37`, `T38`, `T39`, `T40`, `T41`
- 追加: なし
- 次候補: なし (ここまでで全振る舞いをカバー)

### コミット

`feat(setup): integrate dialoguer prompts with trait abstraction and wizard flow tests`

---

## Step 9: 旧 TUI コード削除 - SetupApp / draw_* / handle_* / init_terminal の一括削除

### この Step の目的

`src/setup/mod.rs` から ratatui / crossterm に依存する全コードを削除し、`run_setup_wizard` を `wizard::run` に委譲する。

### 今回選ぶ項目

- 対象: なし (削除 Step)
- 選ぶ理由: 新フローが完成した後なので、旧コードを安全に削除できる
- この時点では扱わないこと: 新機能追加

### GREEN: 最小実装

- 削除対象 (`src/setup/mod.rs`):
  - `Field`, `SetupMode`, `SelectorState`, `SelectorItem`, `SetupApp` 構造体
  - `SetupApp::new`, `load_existing_config` (旧), `visible_fields`, `move_selection`, `current_field*`, `save`
  - `init_terminal`, `run_loop`, `run_inner`, `read_setup_key`, `handle_setup_key`, `handle_selector_key`, `handle_edit_key`, `handle_navigate_key`, `enter_navigate_mode`, `finish_editing`, `save_setup`, `open_current_field`, `apply_selector_value`, `move_selector_selection`, `clamp_selector_selection`, `parse_bool`
  - `draw`, `max_label_width`, `draw_fields`, `draw_selector_popup`
  - crossterm / ratatui の `use` 文
  - 旧テスト (`load_existing_config_reads_*`, `filtered_items_*`, `setup_mode_navigate_default`, `selector_state_holds_original_value`)
- `run_setup_wizard` は残置、`wizard::run(config_path)` へ委譲する thin wrapper に
- 削除対象 (`src/setup/channels.rs`):
  - `update_field_visibility` (Field 構造体廃止で不要)
- 削除対象 (`src/setup/provider.rs`):
  - `SelectorItem`/`SelectorState` への依存部 (`provider_selector_items`, `model_selector_items`, `enter_selector`, `apply_selector_selection`)
  - 残置: `PROVIDER_PRESETS`, `find_provider_preset`, `normalize_provider_id`, `provider_label_for`, `provider_default_base_url`, `provider_default_model`
- 削除対象 (`src/setup/summary.rs`):
  - `validate_fields` (旧), `save_config` (旧), `draw_completion_summary` (ratatui 依存), `mask_secret` は `prompts.rs` or `wizard.rs` へ移動してもよい
  - 残置: `validate_inputs`, `save_config` (新), `backup_config`, `cleanup_old_backups`, `parse_existing_config`, `extract_existing_state_root`

### REFACTOR: 設計の整理

- `cargo check` / `cargo clippy --all-targets --all-features -- -D warnings` が通ることを確認
- デッドコード (`#[allow(dead_code)]` は AGENTS.md で禁止) が出ないよう、完全削除
- 公開範囲: 新設関数は `pub(crate)` または `pub(super)` で最小化
- 依存: `Cargo.toml` から `ratatui`, `crossterm` は**残置** (`src/channels/tui.rs` が使うため)

### テストリスト更新

- 完了: なし
- 追加: 削除による回帰がないか、Step 1〜7 のテストが全て通ることを確認
- 次候補: Step 10 (docs)

### コミット

`refactor(setup): remove legacy ratatui TUI implementation`

---

## Step 10: docs 更新

### この Step の目的

`docs/setup-redesign.md`, `docs/commands.md §1.1`, `docs/config.md §7`, `README.md` を更新する。

### 今回選ぶ項目

- 対象: なし (docs Step)
- 選ぶ理由: 実装完了に伴う文書整合

### GREEN: 最小実装

- `docs/setup-redesign.md`: Status を「設計段階 (未実装)」→「実装済み」へ。Date 更新
- `docs/commands.md §1.1`: `egopulse setup` 行の説明を「対話型設定ウィザード (TUI)」→「対話型設定プロンプト」へ
- `docs/config.md §7`: 全面書き換え。新フロー (Welcome → Q1〜Q7 → Review → Save → Additional Options → Done)、新設定可能項目、設定対象外項目
- `README.md`: Getting Started の `egopulse setup` 記載を確認、TUI 言及があれば「対話型設定プロンプト」へ

### REFACTOR: 設計の整理

- 各 docs 間の相互リンクを確認 (commands.md ↔ config.md ↔ setup-redesign.md)
- 文書スタイルを既存 docs に合わせる (日本語、MECE、テーブル多用)

### コミット

`docs(setup): refresh setup wizard docs`

---

## Step 11: 動作確認

### 自動テスト・Lint

- `cargo fmt --check`
- `cargo test`
- `cargo check`
- `cargo clippy --all-targets --all-features -- -D warnings`

### 失敗時に戻る Step

該当 TDD Cycle (Step 1〜9)。

### E2E 手動確認の扱い

本 Plan では E2E 手動確認は実施しない。Step 8 の `PromptSource` / `OutputSink` trait 抽象 + モック駆動テスト (T35〜T41) で実質的なフロー検証を機械的に担保済みのため。実機での対話確認 (`egopulse setup` を実際に起動して dialoguer 入力を試すこと) は **ユーザーが自身の環境で実施する**。AI 側では `~/.egopulse/` 配下を一切触らない。

---

## Step 12: Plan・仕様書との自己チェック

実装完了後にこの Plan と関連仕様書を最初から読み直し、実装・自動テスト・文書が要求した振る舞いと一致しているかを照合する。未実装、過剰実装、テスト不足、仕様書との齟齬を見つけた場合は、該当する TDD Cycle へ戻って修正し、動作確認を再実行してからこの Step を完了する。

- Plan のテストリストと各 Cycle が完了条件を満たしている (T1〜T41 すべて「完了」)
- `docs/setup-redesign.md` の What と実装結果が一致している:
  - Q1〜Q7 のフロー順序
  - Agent Label → slugify → agent id の仕様
  - Review の3択 (StartOver / Abort / SaveAnyway)
  - Additional Options のカテゴリ (System / Web UI / Channels / Subsystems)
  - Done の Web/Discord/Telegram 案内
  - **既存設定の再編集 (§5.3)**: 各 prompt の default 事前入力、`WEB_AUTH_TOKEN` と `state_root` の保持
- 実装中に変更した設計判断が関連 docs へ反映されている (`docs/commands.md`, `docs/config.md §7`)
- 変更ファイル一覧、コミット分割、自動テスト一覧が実際の変更と一致している
- 禁止事項の確認: `#[allow(dead_code)]` を使っていない、型エラー抑止 (`as any` 等の Rust 版、`unwrap()` 乱用等) がない

---

## Step 13: PR 作成

- PR タイトル: `feat: refresh setup wizard to chat-like dialoguer flow`
- PR description (日本語):
  - 概要: `egopulse setup` を ratatui フル TUI から dialoguer ベースのチャットライク順次プロンプトへ全面刷新。設計元 `docs/setup-redesign.md`
  - 変更ポイント:
    - Agent-First フロー (Q1 Agent Label → Q2 Provider → ...)
    - Web チャネル強制有効化を廃止、ユーザー選択に
    - Review での3択 (StartOver/Abort/SaveAnyway)
    - Additional Options ステップ新設
    - 既存資産 (PROVIDER_PRESETS, save_config, backup_config) は流用・拡張
    - 約 900 行削除 (旧 TUI 実装)
  - テスト: T1〜T41 のユニットテスト (E2E 手動確認はユーザー側で実施)
  - Close #<issue-number> (該当 Issue がある場合)
- レビューは Coderabbit が自動対応 (PR 作成後10分程度)。レビューバックは `pr-review-back-workflow` skill を使用

---

## 変更ファイル一覧

| ファイル | 変更種別 | 内容 |
| ---- | ---- | -- |
| `src/setup/mod.rs` | 大規模削除 + 一部残置 | 約 900 行削除。`run_setup_wizard` の thin wrapper のみ残置 |
| `src/setup/prompts.rs` | **新規** | dialoguer ラッパー、`format_api_key_for_review` 等の純粋関数 |
| `src/setup/wizard.rs` | **新規** | フロー制御、メッセージビルダー、`ReviewDecision` enum |
| `src/setup/provider.rs` | 一部削除 + 残置 | `PROVIDER_PRESETS`, `find_provider_preset` 等は残置。SelectorItem 依存部は削除 |
| `src/setup/channels.rs` | 拡張 + 一部削除 | `build_channel_configs` に `web_enabled` 追加。`update_field_visibility` は削除 |
| `src/setup/summary.rs` | 拡張 + 一部削除 | `validate_inputs`, 新 `save_config`, `parse_existing_config` 追加。旧 `validate_fields`, 旧 `save_config`, `draw_completion_summary` は削除 |
| `Cargo.toml` | 変更 | `dialoguer` 追加 |
| `docs/setup-redesign.md` | 変更 | Status を「実装済み」へ |
| `docs/commands.md` | 変更 | §1.1 `egopulse setup` 行 |
| `docs/config.md` | 変更 | §7 全面書き換え |
| `README.md` | 変更 | Getting Started の説明整合 |

---

## コミット分割

1. `feat(setup): add slugify_agent_id for agent label normalization` - `src/setup/mod.rs` or `src/setup/slugify.rs` / Step 1
2. `feat(setup): add SetupInputs type and validate_inputs for chat-based wizard` - `src/setup/summary.rs` / Step 2
3. `feat(setup): allow web channel disablement in build_channel_configs` - `src/setup/channels.rs` / Step 3
4. `feat(setup): support agent label, web disablement and existing value preservation in save_config` - `src/setup/summary.rs` / Step 4
5. `refactor(setup): extract parse_existing_config as pure function` - `src/setup/summary.rs` or `src/setup/mod.rs` / Step 5
6. `feat(setup): add format_api_key_for_review for Review step` - `src/setup/prompts.rs` / Step 6
7. `feat(setup): add wizard message builders, review decision and branch predicates` - `src/setup/wizard.rs` / Step 7
8. `feat(setup): integrate dialoguer prompts with trait abstraction and wizard flow tests` - `src/setup/prompts.rs`, `src/setup/wizard.rs`, `Cargo.toml` / Step 8
9. `refactor(setup): remove legacy ratatui TUI implementation` - `src/setup/mod.rs`, `src/setup/provider.rs`, `src/setup/channels.rs`, `src/setup/summary.rs` / Step 9
10. `docs(setup): refresh setup wizard docs` - `docs/*`, `README.md` / Step 10

---

## 自動テスト一覧 (全 41 件)

この一覧は Plan 作成時点で必要と判断した最低限の予定であり、最終テスト件数の上限ではない。実装中に追加された不安には、テストリストと TDD Cycle を追加して対応する。E2E 手動確認は本 Plan スコープ外 (ユーザー側で実施)。

### slugify (Step 1、全 6 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T1 | `slugify_lowercases_ascii_letters` | Step 1 | `cargo test slugify_lowercases` |
| T2 | `slugify_replaces_whitespace_with_hyphen` | Step 1 | `cargo test slugify_replaces` |
| T3 | `slugify_preserves_alphanumeric` | Step 1 | `cargo test slugify_preserves` |
| T4 | `slugify_compresses_consecutive_separators_and_trims` | Step 1 | `cargo test slugify_compresses` |
| T5 | `slugify_falls_back_to_default_for_empty_or_symbols_only` | Step 1 | `cargo test slugify_falls_back` |
| T6 | `slugify_replaces_non_ascii_with_hyphen` | Step 1 | `cargo test slugify_replaces_non_ascii` |

### validate_inputs (Step 2、全 5 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T7 | `validate_inputs_rejects_empty_provider` | Step 2 | `cargo test validate_inputs_rejects_empty_provider` |
| T8 | `validate_inputs_rejects_invalid_base_url` | Step 2 | `cargo test validate_inputs_rejects_invalid_base_url` |
| T9 | `validate_inputs_rejects_discord_enabled_without_token` | Step 2 | `cargo test validate_inputs_rejects_discord` |
| T10 | `validate_inputs_accepts_minimum_set` | Step 2 | `cargo test validate_inputs_accepts_minimum` |
| T11 | `validate_inputs_allows_empty_api_key_for_localhost` | Step 2 | `cargo test validate_inputs_allows_empty_api_key` |

### build_channel_configs (Step 3、全 3 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T12 | `build_channel_configs_includes_web_when_enabled` | Step 3 | `cargo test build_channel_configs_includes_web` |
| T13 | `build_channel_configs_omits_web_when_disabled` | Step 3 | `cargo test build_channel_configs_omits_web` |
| T14 | `build_channel_configs_includes_discord_and_telegram_when_enabled` | Step 3 | `cargo test build_channel_configs_includes_discord` |

### save_config (Step 4、全 7 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T15 | `save_config_persists_agent_label` | Step 4 | `cargo test save_config_persists_agent_label` |
| T16 | `save_config_sets_default_agent_to_user_id` | Step 4 | `cargo test save_config_sets_default_agent` |
| T17 | `save_config_omits_web_entry_when_disabled` | Step 4 | `cargo test save_config_omits_web_entry` |
| T18 | `save_config_creates_backup_when_existing_file_present` | Step 4 | `cargo test save_config_creates_backup` |
| T19 | `save_config_roundtrips_with_config_load` | Step 4 | `cargo test save_config_roundtrips` |
| T20 | `save_config_reuses_existing_web_auth_token` | Step 4 | `cargo test save_config_reuses_existing_web_auth_token` |
| T21 | `save_config_preserves_existing_state_root` | Step 4 | `cargo test save_config_preserves_existing_state_root` |

### parse_existing_config / mask_secret (Step 5、全 3 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T22 | `parse_existing_config_returns_err_for_invalid_yaml` | Step 5 | `cargo test parse_existing_config_returns_err` |
| T23 | `parse_existing_config_extracts_provider_schema` | Step 5 | `cargo test parse_existing_config_extracts` |
| T24 | `mask_secret_fully_masks_short_values` | Step 5 | `cargo test mask_secret_fully_masks` |

### format_api_key_for_review (Step 6、全 2 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T25 | `format_api_key_for_review_masks_long_values` | Step 6 | `cargo test format_api_key_for_review_masks_long` |
| T26 | `format_api_key_for_review_shows_empty_for_blank` | Step 6 | `cargo test format_api_key_for_review_shows_empty` |

### wizard メッセージビルダー + 分岐判断 (Step 7、全 8 件)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T27 | `review_decision_from_index_maps_correctly` | Step 7 | `cargo test review_decision_from_index` |
| T28 | `build_review_summary_renders_all_fields` | Step 7 | `cargo test build_review_summary_renders` |
| T29 | `build_additional_options_text_includes_all_categories` | Step 7 | `cargo test build_additional_options_text` |
| T30 | `build_done_message_includes_next_steps_and_channel_hints` | Step 7 | `cargo test build_done_message_includes` |
| T31 | `should_confirm_empty_api_key_returns_false_for_localhost` | Step 7 | `cargo test should_confirm_empty_api_key_returns_false` |
| T32 | `should_confirm_empty_api_key_returns_true_for_remote` | Step 7 | `cargo test should_confirm_empty_api_key_returns_true` |
| T33 | `is_custom_provider_returns_true_only_for_custom` | Step 7 | `cargo test is_custom_provider_returns_true` |
| T34 | `should_ask_model_as_free_text_returns_true_only_for_custom` | Step 7 | `cargo test should_ask_model_as_free_text` |

### wizard 統合 (Step 8、全 7 件。trait 抽象 + モック駆動)

| テストリストID | 自動テスト名 | 追加Step | 実行コマンド |
| -- | -- | -- | -- |
| T35 | `prefill_defaults_uses_existing_config_values` | Step 8 | `cargo test prefill_defaults_uses_existing` |
| T36 | `wizard_review_startover_returns_to_q1` | Step 8 | `cargo test wizard_review_startover` |
| T37 | `wizard_review_abort_exits_without_save` | Step 8 | `cargo test wizard_review_abort` |
| T38 | `wizard_review_save_anyway_writes_config` | Step 8 | `cargo test wizard_review_save_anyway` |
| T39 | `wizard_review_yes_saves_directly` | Step 8 | `cargo test wizard_review_yes_saves` |
| T40 | `wizard_parse_error_decline_aborts` | Step 8 | `cargo test wizard_parse_error_decline` |
| T41 | `wizard_parse_error_accept_continues` | Step 8 | `cargo test wizard_parse_error_accept` |

---

## 工数見積もり

| Step | 内容 | 見積もり |
| -- | -- | -- |
| Step 0 | Worktree 作成 | ~5 min |
| Step 1 | slugify TDD Cycle (6 テスト) | ~40 行 / 0.5h |
| Step 2 | validate_inputs TDD Cycle (5 テスト) | ~50 行 / 1h |
| Step 3 | build_channel_configs 拡張 TDD Cycle (3 テスト) | ~30 行 / 0.5h |
| Step 4 | save_config 拡張 TDD Cycle (7 テスト) | ~100 行 / 2h |
| Step 5 | parse_existing_config 抽出 TDD Cycle (3 テスト) | ~50 行 / 0.5h |
| Step 6 | format_api_key_for_review TDD Cycle (2 テスト) | ~20 行 / 0.5h |
| Step 7 | wizard メッセージビルダー + 分岐判断 TDD Cycle (8 テスト) | ~220 行 / 2.5h |
| Step 8 | dialoguer 統合 + trait 抽象 + wizard モック駆動テスト (7 テスト) | ~400 行 / 4h |
| Step 9 | 旧 TUI コード削除 | -900 行 / 1h |
| Step 10 | docs 更新 | ~200 行 / 1h |
| Step 11 | 動作確認 (自動テスト・Lint のみ、E2E 手動はユーザー側) | ~0.5h |
| Step 12 | Plan・仕様書との自己チェック | ~0.5h |
| Step 13 | PR 作成 | ~0.5h |
| **合計** | | **~1010 行追加 / ~900 行削除 / ~15h (約 2 営業日)** |
