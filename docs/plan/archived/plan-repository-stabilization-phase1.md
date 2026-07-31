# EgoPulse リポジトリ安定化 Phase 1 作業計画

## 0. 文書情報

| 項目 | 内容 |
|---|---|
| 対象リポジトリ | `endo-ly/egopulse` |
| 基準ブランチ | `main` |
| 基準コミット | `a4dc615ab89ab55c2344acfe859fb40dd12fe03c` |
| Phase | 1 / 3 |
| Phase名称 | 障害の拡大を止める |
| 実施主体 | AIコーディングエージェント |
| Delivery | 1 branch / 1 worktree / 1 PR |
| 推奨配置先 | `docs/plan/plan-repository-stabilization-phase1.md` |
| 完了後の配置先 | `docs/plan/archived/plan-repository-stabilization-phase1.md` |

---

## 1. 背景・問題定義・目的

### 1.1 背景

EgoPulseは、TUI、Web、Discord、Telegram、Webhook、複数Agent、Tool実行、Sleep Batch、Secret Modeを単一Runtimeで扱う段階まで機能が拡張されている。各機能には多数のUnit Testがあり、Storage、LLM response解析、Tool phase、Sleep stepなどの局所的な品質は高い。

一方で、機能追加と局所改善が先行した結果、複数モジュールをまたぐ実行規則が統一されていない。特に「失敗後にどこから再開するか」「何を処理済みとみなすか」「不明なscopeをどう扱うか」「受付済み処理をどこまで保持できるか」「どのcommitをRelease可能とみなすか」というRuntime全体の境界に、fail-openまたは暗黙のfallbackが残っている。

この状態では、通常時のテストが成功していても、LLM障害、セッション数増加、設定ミス、高負荷、downgrade、Release失敗といった非正常系で、重複副作用、長期記憶の取りこぼし、秘密データの通常領域保存、メモリ増加、壊れたReleaseの公開が発生し得る。

### 1.2 現在の問題構造

Phase 1が対象とする問題は、個別の小さなバグではなく、次の共通構造を持つ。

> 正常系では成立するが、境界条件で処理済み状態・identity・scope・capacity・release可否が曖昧になり、システムが安全側ではなく継続側へ倒れる。

| 境界 | 現在の状態 | 起こり得る結果 | Phase 1で行うこと |
|---|---|---|---|
| Turn失敗 | LLMのretryable errorでTurn全体を再実行 | 保存済み入力、Tool、外部送信の重複 | Turn全体の自動再実行を止める |
| Sleep増分 | 候補抽出はrun時刻、処理済み判定はstep checkpoint | 20件上限から漏れたsessionが次回候補から消える | 候補抽出も既存checkpointへ統一する |
| Web identity | 既存chatでもdefault agentを再選択 | 保存済みsessionと異なるpersona/modelで継続 | DBに保存されたagentを使う |
| DB downgrade | binaryより新しいschemaを明示拒否しない | 旧コードが未知schemaを読み書きする | 書込み前にfail closedする |
| Secret境界 | secret収集漏れとscopeのNormal fallback | token露出、Secret intended dataの通常DB保存 | secret網羅とscope解決の厳格化 |
| 高負荷 | queueとorigin trackingが無制限 | 長期稼働でメモリ増加、受付後のsilent loss | 上限、拒否結果、観測性を追加する |
| 品質保証 | Web testはCI対象外、tagはbuild前に作成 | 既知のWeb失敗やbuild失敗を含むRelease | CI成功とartifact成功をRelease条件にする |

### 1.3 なぜPhase 1を先に行うか

Phase 2では`TurnId`、Tool実行台帳、会話イベントの正本、設定世代など、実行モデルと永続化モデルを変更する。Phase 3ではSupervisor、durable job、Memory generation commit、OS sandboxなど、運用基盤とセキュリティ境界を変更する。

しかし現状のまま大規模変更へ進むと、既存の危険なfallbackと新しい基盤が同時に存在し、障害原因と回帰原因の切り分けが難しくなる。また、Phase 2／3の実装中にも現在のRuntimeは運用されるため、その期間に重複実行やデータ取りこぼしが続く。

そこでPhase 1では、恒久基盤を先取りせず、次の順序で土台を安定させる。

1. 現在確認できる危険な挙動を再現テストで固定する。
2. 暗黙のretry／fallback／無制限受付を止め、失敗を明示する。
3. 既存checkpointなど、すでに存在する正しい仕組みへ判定基準を揃える。
4. CIとRelease gateを整え、以後のPhaseで安全保証が後退しないようにする。

Phase 1は「最終設計」ではなく「これ以上状態を悪化させないための安全化」である。

### 1.4 なぜ単一PRで実施するか

Phase 1の変更は、Turn、Sleep、Security、Queue、CIと領域は異なるが、全体として「Phase 2へ進む前のRuntime安全基準」を構成する。部分的にmergeすると、たとえば危険なretryだけ撤去されてCI gateが未導入、queue rejectionだけ追加されてWebhookが成功扱いを続ける、といった中間状態がmainへ残る。

そのため、Phase 1は1つのbranchと1つのPRでまとめて導入する。ただしレビュー不能な巨大差分にしないため、内部を6つの作業パッケージと論理commitへ分割し、各Packageを独立に検証可能にする。mergeの原子性はPRで、レビュー可能性とrollback可能性はcommitで担保する。

### 1.5 目的

本Phaseの目的は、EgoPulseの非正常系を「継続できるかもしれない挙動」から「状態を壊さず、原因を観測できる挙動」へ変更することである。

Phase 1完了時に、次の保証を成立させる。

1. 一度開始したAgent Turnを、LLMエラーだけを理由に先頭から自動再実行しない。
2. Sleep対象が20セッションを超えても、未処理セッションがcheckpoint上から消えない。
3. Webの既存セッションでは、そのセッションに保存されたAgentを使用する。
4. バイナリが対応している版より新しいDBを開いた場合、書込み前に明示的に起動失敗する。
5. Runtime設定内の既知の秘密値が、Tool Resultの永続化前にRedaction対象へ入る。
6. Discord／Telegram向けWebhookのscope解決に失敗した場合、Normalへ黙って降格しない。
7. Turn queueとorigin trackingが無制限に増加しない。
8. Webのtest・typecheck・buildをCI必須条件にする。
9. CI不成功、またはartifact build不成功のcommitにRelease tagを作らない。

### 1.6 完了後の状態

Phase 1完了後もLLM一時障害は発生し、queue fullでは入力が拒否され、未登録Webhook targetは設定エラーになる。これは可用性の低下ではなく、冪等性やdurabilityが未完成な状態で安全を偽装しないための意図的な仕様である。

利用者と運用者は、処理が成功したのか、拒否されたのか、失敗したのかを区別できる。未処理データはcheckpoint上に残り、未知のDBやscopeを推測で処理せず、Release可能性は自動検証結果によって決まる。この状態をPhase 2／3の開始条件とする。

---

## 2. スコープ

### 2.1 実施対象

- 現在失敗しているWebテスト2件の仕様確定と修正
- Web既存セッションのAgent選択修正
- 通常DB／Secret DBのforward-version guard
- Turn全体リトライの撤去
- Sleep候補抽出を既存のステップ別checkpoint基準へ統一
- Secret収集対象の網羅
- Webhook target scopeのfail-closed化
- per-session／global Turn queue上限
- TurnTrackerの有界化
- queue rejectionの呼出元通知、ログ、メトリクス
- Web品質ゲートのCI追加
- Release workflowのCI成功後起動とtag作成順序修正
- 上記に対応する仕様文書更新

### 2.2 明示的な対象外

以下はPhase 2またはPhase 3で扱う。Phase 1の実装中に先回りして導入しない。

- `TurnId`、Tool実行台帳、Tool副作用の完全な冪等化
- `conversation_events` を正本とする会話イベントモデル
- `sessions.messages_json`／`messages`／`tool_calls` の統合
- 世代付きConfigManager、`BootConfig`／`LiveConfig` 分離
- Runtime全体の`JoinSet`／`CancellationToken` Supervisor
- Webhook jobの永続キュー化とstatus API
- Sleep MemoryファイルとDBのgeneration commit protocol
- Shell/File Toolのコンテナ・namespace隔離
- `AppState`の全面分解
- Discord／Telegramの共通Inbound Policy全面抽出
- `build.rs`と組み込み文書の全面再設計

Phase 1では、上記の恒久設計を必要とするほど変更範囲を広げない。

---

## 3. 実装原則

### 3.1 安全性の優先順位

競合する場合は次の順で判断する。

1. 重複副作用、秘密漏洩、DB破損を起こさない
2. 未処理状態を不可視化しない
3. 拒否や失敗を呼出元・ログ・メトリクスから観測できる
4. 一時的な可用性低下を避ける

したがって、Phase 1では「危険な自動リトライを残す」より「一度失敗を返す」を選ぶ。また、容量超過時は無制限に蓄積せず明示的に拒否する。

### 3.2 互換フォールバック禁止

- Agent解決失敗時に`default_agent`へ戻さない。
- Secret scope解決失敗時に`Normal`へ戻さない。
- 新しいDB versionを現在版として扱わない。
- queue fullを成功扱いしない。
- テストを通す目的で期待値だけを実装に合わせない。

### 3.3 TDD

各作業パッケージは次の順で進める。

1. 問題を再現するテストを追加する。
2. 追加したテスト単体が意図した理由で失敗することを確認する。
3. 最小のproduction codeで通す。
4. 関連テストを追加して境界条件を固定する。
5. 対象モジュールのテストを通す。
6. 全体品質ゲートを通す。

テストはリポジトリ規約どおりAAA形式とし、テスト名から保証内容が分かるようにする。

---

## 4. 単一PR構成と実施順

Phase 1は1つのbranch、1つのGit Worktree、1つのPRで実施する。

| 項目 | 内容 |
|---|---|
| Branch | `fix/repository-stabilization-phase1` |
| PR title案 | `fix: stabilize turn, sleep, security, queue, and release boundaries` |
| PR description | 日本語 |
| Merge単位 | Phase 1全体を一括merge |
| Rollback単位 | 作業パッケージごとに分けたcommit |

PRを分割しない代わりに、実装を6つの作業パッケージへ分ける。各パッケージを完了するたびに対象テスト、差分レビュー、Plan照合、独立commitまで行い、その後に次へ進む。

| 順序 | 作業パッケージ | 主目的 | 依存 |
|---:|---|---|---|
| 1 | Correctness Guards | Web回帰修正、Agent選択、DB版数guard | なし |
| 2 | Turn Retry Safety | Turn全体の自動再実行撤去 | Package 1 |
| 3 | Sleep Pending Session Selection | Sleep候補をcheckpoint基準へ統一 | Package 2 |
| 4 | Runtime Secret Boundaries | Secret網羅、Webhook scope fail-closed | Package 3 |
| 5 | Turn Queue Backpressure | queueとTurnTrackerの有界化 | Package 4 |
| 6 | CI and Release Quality Gate | Web CIとRelease gate | Package 5 |

### 4.1 Commit構成

最低限、次の論理commitへ分割する。1つの作業パッケージが大きい場合は、テストと実装を同じ意味単位の複数commitに分けてよい。

1. `fix: add phase one correctness guards`
2. `fix: prevent unsafe whole-turn retries`
3. `fix: select sleep backlog from step checkpoints`
4. `fix: close runtime secret boundary gaps`
5. `fix: add turn queue backpressure`
6. `ci: gate releases on complete quality checks`
7. `docs: align phase one runtime guarantees`

各commitは単独でbuild可能かつテスト可能にする。後続commitで一時的な壊れた状態を直す前提のcommitを作らない。履歴を見れば、どの保証をどの差分が導入したか追跡できる状態にする。

### 4.2 単一PRのレビュー運用

- 実装開始時に1つのDraft PRを作成してよい。作成しない場合も最終的なPRは1つだけとする。
- 作業パッケージ完了ごとにPR descriptionのチェックリストと検証結果を更新する。
- レビュー依頼は全パッケージと全体検証の完了後に行う。
- レビュアーがcommit単位で追えるよう、無関係なfixupを別パッケージのcommitへ混在させない。
- Phase 1の一部だけを先にmergeしない。重大な問題で一部を取り下げる場合は、該当commitを明示的にrevertし、PlanとPR descriptionからもscopeを更新する。
- 複数のAIエージェントを使う場合も、同一worktreeを同時編集させない。委譲はread-only調査または明確に非重複なレビューに限定する。

---

## 5. Work Package 1 — Correctness Guards

### 5.1 背景と目的

Web UIには、error状態のTool Cardを自動展開するというテストと、常に閉じた状態から始まる実装の不一致がある。また、session keyテストはUTCの日時文字列を渡しながらlocal timeの結果を期待しており、実行環境のtimezoneによって失敗する。これらは単にテストが2件落ちている問題ではなく、WebテストがCIに含まれていないため、仕様と実装の不一致がmain上で検出されない状態を示している。

Webの既存session解決では、DBに保存された`agent_id`ではなく`default_agent`を`SurfaceContext`へ設定する。このため、default以外のAgentで開始した会話を再開すると、同じchat IDのままpersona、model、memoryの主体だけが変わり得る。会話identityの破壊であり、表示上は正常に見えるため発見しにくい。

DB migrationでは、現在より古いschemaは移行する一方、binaryより新しいschemaを明示的に拒否しない。self-update後のrollbackや古いbinaryの再起動時に、未知のschemaを旧コードが読み書きする可能性がある。

このPackageでは、後続の安全化を検証できるbaselineを作るため、既知のWeb仕様不一致を解消し、session identityとDB versionを推測ではなく保存値・対応versionから決定する。

### 5.2 対象ファイル候補

- `web/src/features/chat/ToolCard.tsx`
- `web/src/features/chat/__tests__/ToolCard.test.tsx`
- `web/src/shared/api/sessions.ts`
- `web/src/shared/api/__tests__/client.test.ts`
- `src/channels/web/stream.rs`
- `src/storage/migration.rs`
- `src/error.rs`
- `docs/api.md`
- `docs/channels.md`
- `docs/db.md`

実際の責務に応じてテストを別ファイルへ分離してよいが、公開APIは増やさない。

### 5.3 作業1：Tool error cardの仕様固定

期待仕様：

- 初期状態が`error`なら詳細を自動展開する。
- `pending`または`success`から`error`へ遷移した場合も一度自動展開する。
- 自動展開後、ユーザーは手動で閉じられる。
- `defaultExpanded`による通常状態の初期値は維持する。

実装は`event.state`の変化を監視して`error`遷移時に展開する。`error`中ずっと展開を強制する派生値にはせず、ユーザーの手動closeを可能にする。

追加・更新テスト：

- `tool_card_initial_error_auto_expands`
- `tool_card_error_transition_auto_expands`
- `tool_card_can_be_collapsed_after_error_auto_expand`
- 既存pending／success／toggleテスト

### 5.4 作業2：session keyテストのtimezone非依存化

`createSessionKey`はブラウザのlocal timeを使用する現仕様を維持する。UTC文字列を与えて固定時刻を期待するテストをやめ、local constructorで時刻を作る。

確認条件：

```bash
TZ=UTC npm --prefix web test
TZ=Asia/Tokyo npm --prefix web test
```

両方で同じテスト結果になること。production codeをUTCへ変更して問題を隠さない。

### 5.5 作業3：Web既存セッションのAgent選択

既存の`chat:{id}`を解決した場合、`state.app_state.config.default_agent`ではなく、DBから取得した`ChatInfo.agent_id`を`SurfaceContext`へ設定する。

新規セッションだけが`default_agent`を使用する。

テスト容易性のため、必要であれば`ChatInfo`から`SurfaceContext`を構築するprivate helperを抽出する。Agent ID以外のchannel、thread、chat typeもDB値から維持されることを同じテストで確認する。

追加テスト：

- default agentと異なるAgentを持つchatを作る。
- `chat:{id}`を指定してrun開始前のcontextを解決する。
- `context.agent_id == chat.agent_id`を確認する。
- 新規Web sessionは引き続きdefault agentを使用することを確認する。

### 5.6 作業4：forward schema version guard

通常DBとSecret DBのmigration開始直後に、次を判定する。

```text
found_version > supported_version => structured StorageErrorを返す
```

要件：

- guardはDDL／DMLより前に実行する。
- `debug_assert`を実行時保証として使用しない。
- errorにはDB種別、検出version、対応versionを含める。
- error表示は既存規約どおりlower-caseの識別可能な形式にする。
- 新しいversionを現在versionへ書き戻さない。

追加テスト：

- 通常DBのversionを`SCHEMA_VERSION + 1`に設定し、`run_migrations`が失敗する。
- Secret DBのversionを`SECRET_SCHEMA_VERSION + 1`に設定し、`run_secret_migrations`が失敗する。
- guard失敗後もversionと既存データが不変である。
- current versionの再実行は成功する。
- older versionの既存migrationテストはすべて成功する。

### 5.7 Package 1受入条件

- Webテストがtimezoneに依存せず全件成功する。
- 既存Web chatが保存済みAgentで実行される。
- 通常DB／Secret DBのfuture versionが書込み前に拒否される。
- 関連するAPI、Channel、DB文書が実装と一致する。

---

## 6. Work Package 2 — Turn Retry Safety

### 6.1 背景と目的

現在のRuntimeは、429、5xx、network errorなどをretryableと判定すると、`process_turn_with_events`全体を呼び直す。しかしTurn内部では、最初のLLM requestより前にユーザー入力をDBとsessionへ保存し、model loopの途中ではToolを実行して結果を保存する。

したがって外側のretryはHTTP requestの再試行ではない。すでにcommitされた入力と副作用を含む業務処理全体の再実行である。最初のLLM request失敗でもuser messageが重複し、Tool後のLLM request失敗ではShell、file write、Agent Send、channel送信などが再度実行され得る。

Phase 2で`TurnId`、iteration境界、Tool実行台帳を導入するまでは、安全に再開できる位置をRuntimeが判断できない。このPackageでは一時的な可用性より状態の一意性を優先し、Turn全体の自動再実行を撤去する。目的はretry機能の削除そのものではなく、「再試行可能な単位が定義されるまで、commit済み処理を再実行しない」という保証を作ることである。

### 6.2 対象ファイル候補

- `src/runtime/mod.rs`
- `src/agent_loop/turn.rs`または既存test utility
- `src/runtime/metrics.rs`
- `docs/session-lifecycle.md`
- `docs/channels.md`

### 6.3 実装方針

- `MAX_TURN_RETRIES`と`run_retry_loop`による`process_turn_with_events`全体の反復を削除する。
- 1回の受付入力につき`process_turn_with_events`を最大1回だけ呼ぶ。
- `ToolProgressCoordinator`の起動、sender drop、timeout、abort処理は維持する。
- Codex 401時にtoken refreshを行う場合も、現在のTurnは再実行しない。refreshは次回Turnのための状態更新に限定し、現在のerrorを返す。
- function名、doc comment、ログ、メトリクスから「安全なretryが存在する」と誤解させる表現を除去する。
- Provider内部で、まだレスポンスも副作用も確定していない単一HTTP requestを再試行する既存処理は、このPRでは変更しない。

Phase 2で`TurnId`とTool実行台帳を導入するまで、Turn全体の自動再実行は復活させない。

### 6.4 必須テスト

最低限、次の2段階をテストする。

#### ケースA：最初のLLM呼出しがretryable error

- 1回の入力を実行する。
- mock providerはretryable errorを返す。
- provider call countが1である。
- user messageの保存件数が1である。
- 呼出元へerrorが返る。

#### ケースB：Tool実行後のLLM呼出しがretryable error

- 1回目のLLM応答でTool Callを返す。
- 副作用回数を`AtomicUsize`等で観測できるtest toolを実行する。
- 2回目のLLM呼出しでretryable errorを返す。
- Tool実行回数が1である。
- user messageが重複しない。
- Turn全体が再開されない。

既存の「retry後に成功する」ことを期待するテストは、新しい安全仕様に合わせて削除または置換する。単に期待値だけを変えず、重複しないことを明示的に検証する。

### 6.5 観測性

- transient errorと最終turn failureは既存のerror kindで観測可能にする。
- ログに「retrying」と出さない。
- retry回数metricが存在する場合は、意味がなくなるため削除する。
- 自動retryを止めたことをsession lifecycle文書へ記載する。

### 6.6 Package 2受入条件

- `process_turn_with_events`全体を反復する経路が存在しない。
- retryable errorでもユーザー入力とTool副作用が1回を超えないテストがある。
- 既存の正常Turn、Tool progress、failure通知が維持される。

---

## 7. Work Package 3 — Sleep Pending Session Selection

### 7.1 背景と目的

Sleepの各stepは、セッション別のmessage checkpointを持ち、成功したstepだけcursorを進める。この仕組み自体は、失敗runからの再処理とstep単位の増分実行を正しく表現している。

一方、Sleepを開始するかどうかの判定と対象sessionの抽出には、最新成功runの`finished_at`が使われる。さらに1runの対象は最新20セッションに制限される。この2つのcursorモデルが一致していないため、20件から漏れたsessionはcheckpoint上では未処理のままでも、次回の候補検索では最新成功時刻より古いとして見えなくなる。継続的に更新されるhot sessionが常に上位20件へ入る場合、古いsessionの処理は長期間飢餓する。

このPackageでは20件上限を入力サイズ制御として維持しつつ、候補抽出と未処理件数も既存checkpointから導く。これにより「何を処理済みとするか」をSleep全体で一つの規則へ揃え、未処理sessionがrun時刻の更新によって不可視化されることを防ぐ。

### 7.2 背景上の制約

既存実装には`MAX_SOURCE_SESSIONS = 20`とセッション別・ステップ別checkpointがある。20件上限は1回のSleep runの入力制限として維持してよい。

問題は上限そのものではなく、候補外となったセッションをglobal `finished_at`より古いという理由で次回候補から除外することである。

### 7.3 対象ファイル候補

- `src/sleep/orchestrator.rs`
- `src/storage/sleep.rs`
- 必要な場合のみ`src/storage/chat.rs`
- `docs/sleep.md`
- `docs/db.md`

新しいcheckpoint tableは追加しない。既存の`sleep_step_checkpoints`を利用する。

### 7.4 ストレージAPI

責務が分かる名前で、次に相当するqueryを追加する。

- `count_agent_pending_sleep_messages(agent_id)`
- `get_agent_sessions_with_pending_sleep_messages(agent_id, limit)`

メッセージ`m`は、次のどちらかを満たすとpendingとする。

- Event Extractionの該当セッションcheckpointが存在しない、または`(m.timestamp, m.id)`がcheckpointより新しい。
- Prospective Updateの該当セッションcheckpointが存在しない、または`(m.timestamp, m.id)`がcheckpointより新しい。

同じmessageが両方のstepでpendingでも、pending件数では1件として数える。

候補セッションは「最古のpending messageが古い順、同値ならchat_id順」で決定する。最新セッション優先にしない。継続的に更新されるhot sessionが古いbacklogを飢餓させないためである。

### 7.5 orchestrator変更

- `collect_sleep_input`から最新成功runの`finished_at`依存を外す。
- skip thresholdはpending message総数に対して適用する。
- `MAX_SOURCE_SESSIONS`は1runの上限として維持する。
- 成功したstepだけがcheckpointを進める既存原則を維持する。
- 失敗runでは候補が次回もpendingとして見えることを確認する。
- `source_chats_json`には実際に選んだセッションだけを保存する。

### 7.6 必須テスト

#### 20件超のdrain

- 25セッションそれぞれに`SKIP_THRESHOLD + 1`件のpending messageを作り、1回目の処理後にも残り5セッションの合計がthresholdを超える状態にする。
- 1回目は最大20セッションを処理する。
- 2回目以降で残り5セッションを選べることを確認する。
- 全run後、25セッションすべてで両stepのcheckpointが進んでいることを確認する。

#### hot sessionによる飢餓防止

- 1回目で処理されたセッションへ新しいmessageを追加する。
- 未処理の古い5セッションが、更新されたhot sessionより先に選ばれることを確認する。

#### failure時

- Sleep stepを失敗させる。
- 該当checkpointが進まないことを確認する。
- 次回collectionでも同じmessageがpendingに残ることを確認する。

#### threshold境界

- pending件数が`SKIP_THRESHOLD`以下ならskipする。
- `SKIP_THRESHOLD + 1`ならproceedする。
- 以前処理済みのmessageは件数に含めない。

### 7.7 性能確認

- `EXPLAIN QUERY PLAN`でmessagesとcheckpointの既存index利用を確認する。
- table全体scanが避けられず実データ増加で問題になる場合のみ、queryに対応したindexを追加する。
- 推測だけでindexやschema migrationを追加しない。

### 7.8 Package 3受入条件

- `finished_at`はrunの監査時刻として残るが、pending判定のcursorとして使われない。
- 20件超でも未処理セッションが次回候補から不可視化されない。
- hot sessionが古いbacklogを飢餓させない。
- Sleep、DB文書がcheckpoint基準の仕様になっている。

---

## 8. Work Package 4 — Runtime Secret Boundaries

### 8.1 背景と目的

Tool ResultはLLM、Web履歴、ログ、DBなど複数の経路へ流れるため、Runtimeはconfigから秘密値を収集し、永続化や表示の前にRedactionする。しかし現在の収集対象はProvider API key、Channel auth token、Discord bot tokenなどに限られ、Telegram bot tokenとWebhook receiver tokenが同じ保護対象に入っていない。Redaction処理自体が正しくても、秘密値のregistryが不完全なら漏洩防止は成立しない。

Webhookのtarget scopeはDiscord／Telegram設定から解決するが、threadのparse失敗、未登録thread、channel map欠落時に`Normal`へfallbackする。Secret channelを意図した設定ミスでも通常DBへ会話を保存できるため、可用性のためのfallbackが保存境界の変更になっている。

このPackageでは、Runtimeが既に認識している秘密値を同じRedaction registryへ網羅し、scopeを解決できないWebhookを受付前に拒否する。目的は現在のソフトウェア境界内で明確な漏れを塞ぐことである。

このPackageはShell sandboxを導入するものではない。Prompt Injectionに対する強制的なOS境界はPhase 3で扱い、Phase 1のsecurity文書では保証範囲を過大に表現しない。

### 8.2 対象ファイル候補

- `src/tools/sanitizer.rs`
- `src/config/loader.rs`
- `src/config/resolve.rs`またはscope解決を置く適切なconfig module
- `src/webhooks/handler.rs`
- `src/error.rs`
- `docs/security.md`
- `docs/config.md`
- `docs/channels.md`

### 8.3 Secret収集対象

`collect_config_secrets`で少なくとも次を収集する。

- `providers.<provider>.api_key`
- `channels.<channel>.auth_token`
- Discord bot token
- Telegram bot token
- Webhook receiver token
- OpenAI Codex bearer token（該当provider利用時）

要件：

- `ResolvedValue::value()`から実値を取得する。
- `file_token`／`file_auth_token`のYAML表現を秘密値の正本として使わない。
- 空文字列を登録しない。
- 同じ秘密値が複数経路に存在する場合の置換結果を決定的にする。必要なら値でdeduplicateする。
- Redaction labelは設定パスを特定できるが、秘密値自体を含まない。

必須テスト：

- Provider API key
- Channel auth token
- Discord bot token
- Telegram bot token
- Webhook receiver token
- Codex bearer token
- 複数secretを含むstring／JSON details／LLM content
- 空値と重複値

### 8.4 Webhook scopeのfail-closed化

現在の`resolve_target_scope`にある`unwrap_or(ConversationScope::Normal)`を廃止する。

期待仕様：

- Discord targetはthread IDが数値としてparseでき、設定済みDiscord channelとして解決できる必要がある。
- Telegram targetも同様に設定済みTelegram channelとして解決できる必要がある。
- 解決できたchannelの`secret`値からscopeを決める。
- Web targetは明示的にNormalとする。
- scope概念を持たない対応channelは、match上で明示的にNormalとする。
- 不正ID、未登録thread、欠落channel mapはConfig loadまたはrequest受付時にstructured errorとして拒否する。
- 不明な場合にNormalへ降格しない。

scope resolverは`Result<ConversationScope, ...>`を返す単一実装にし、config validationとrequest処理で同じ規則を使用する。類似判定を2箇所に複製しない。

必須テスト：

- Secret Discord／Telegram targetがSecretになる。
- Normal Discord／Telegram targetがNormalになる。
- parse不能threadを拒否する。
- 未登録threadを拒否する。
- channel map欠落を拒否する。
- Web targetがNormalになる。
- 拒否時にAgent Turnがenqueueされない。

### 8.5 文書上の表現

`docs/security.md`には、Phase 1時点のRedactionは既知の設定secretに対する防御であり、ShellのOS sandboxではないことを明記する。「Prompt Injectionからの完全なexfiltration防止」と誤読できる保証は記載しない。

### 8.6 Package 4受入条件

- Telegram botとWebhook tokenを含む既知secretがRedaction testに入る。
- Discord／Telegram Webhookのscope不明状態をNormalとして処理する経路がない。
- 対象外であるShell sandboxをこの作業パッケージへ混在させていない。

---

## 9. Work Package 5 — Turn Queue Backpressure

### 9.1 背景と目的

同一sessionのTurn順序を守るため、`TurnScheduler`は処理中sessionへの入力を`VecDeque`へ積むが、per-sessionにもRuntime全体にも上限がない。またAgent chainの暴走防止に使う`TurnTracker`はoriginごとのcountとterminal reasonを保持するが、完了後の削除やTTLがない。

通常の少量利用では問題にならないが、Webhook burst、channel連投、停止したLLM、長期連続運転が重なると、処理能力を超えた入力を受付し続け、queueとMapが増加する。Webhookは202を返した後にin-memory queueへ置かれるため、容量という概念がなければ「accepted」の意味も定義できない。

このPackageでは、Phase 3のdurable queueとSupervisorを待たず、現在のin-memory schedulerに明示的な容量を設ける。容量内の順序保証は維持し、超過時は受付拒否、structured log、metric、可能な場合の呼出元通知へ変える。目的は、過負荷を遅延として無制限に隠すのではなく、有限資源の境界として観測可能にすることである。

### 9.2 対象ファイル候補

- `src/runtime/turn_scheduler.rs`
- `src/runtime/channel_input.rs`
- `src/runtime/mod.rs`
- `src/runtime/metrics.rs`
- `src/webhooks/handler.rs`
- 各`submit_agent_turn`呼出元
- `docs/session-lifecycle.md`
- `docs/api.md`
- `docs/channels.md`

### 9.3 Phase 1固定ポリシー

Config hot reloadを広げないため、Phase 1ではprivate constantとして次を導入する。

| 制限 | 初期値 |
|---|---:|
| 1 sessionあたりのqueued turn | 32 |
| Runtime全体のqueued turn | 512 |
| 追跡するorigin | 4096 |
| completed／terminal origin保持TTL | 24時間 |

値には理由をdoc commentで記載する。Phase 2以降で実測に基づきconfig化できるが、このPRでは設定項目を増やさない。

### 9.4 TurnScheduler

`submit`の`Option`だけではqueuedとrejectedを区別できないため、専用結果型を導入する。

概念上の結果：

```text
Started(turn)
Queued
Rejected(SessionQueueFull | GlobalQueueFull)
```

要件：

- per-session上限とglobal上限を同じlock内で判定する。
- dequeue時にglobal queue countを必ず減らす。
- sessionがidleになったらslotを削除する。
- rejection時にqueueへ追加しない。
- lock保持中にasync処理を行わない。
- queue depthとrejection reasonをmetricsへ記録する。

### 9.5 TurnTracker

`counts`と`terminal_reasons`の別Mapを、1つのorigin stateへ統合する。

origin stateは少なくとも次を持つ。

- turn count
- terminal reason
- last touched monotonic time

要件：

- 操作時にTTL超過entryをpruneする。
- 上限到達時に、TTL内のentryを黙ってevictしてchain guardをresetしない。
- prune後も上限なら新しいoriginを明示的に拒否する。
- 既存originの更新は上限到達中も可能にする。
- active chainがTTLを超える可能性をログから判別できるようにする。

Phase 3のSupervisorで正確なorigin lifecycleを所有するまで、TTLは暫定的な有界化として扱う。

### 9.6 rejection伝播

- `submit_agent_turn`は結果を返し、呼出元がStarted／Queued／Rejectedを区別できるようにする。
- Webhookはqueue full時に`429 Too Many Requests`とmachine-readable error codeを返し、202を返さない。
- Channel入力で即時応答可能な場合はユーザーへbusyを通知する。
- Agent Send経由の非同期rejectionは少なくともsystem event、structured log、metricへ記録し、silent dropしない。
- `origin_id`、入力本文、secretをmetric labelへ入れない。

Phase 1ではWebhook jobを永続化しない。202は「in-memory schedulerへの受付成功」を意味することをAPI文書に明記する。

### 9.7 必須テスト

- idle sessionの最初のTurnはStartedになる。
- busy sessionの上限まではQueuedになる。
- per-session上限超過はSessionQueueFullになる。
- 複数session合計でglobal上限超過はGlobalQueueFullになる。
- dequeueごとにglobal countが減る。
- drain後にslotが削除される。
- rejected turnは後から実行されない。
- stale originがTTLで削除される。
- tracker上限で新規originを拒否し、既存originは維持される。
- Webhook queue full時は429で、202にならない。
- terminal originの後続Turnを止める既存chain guardが維持される。

テストでwall clock sleepを使用しない。clock注入または明示的な`Instant`引数を持つprivate helperを用いる。

### 9.8 Package 5受入条件

- queueとtrackerのメモリ使用量にコード上の上限がある。
- capacity超過を成功扱い、またはsilent dropする経路がない。
- 既存のsession内順序保証とAgent chain上限が維持される。

---

## 10. Work Package 6 — CI and Release Quality Gate

### 10.1 背景と目的

現在のCIはRustのfmt、check、clippy、test、doc、audit、denyを実行する一方、Webのtest、typecheck、buildを品質ゲートとして実行しない。実際にmain基準のWebテストには2件の失敗があり、Web production buildが成功してもUI仕様の回帰は検出されない。

Release workflowはmainへのpushでCIとは独立に起動し、Web／Rust artifactのbuildより前にtagを作成・pushする。したがってCI失敗またはartifact build失敗が後から判明しても、Release可能でないcommitを指すtagがすでに残る。頻繁なself-updateと一方向DB migrationを持つRuntimeでは、不完全なReleaseは単なる配布ミスではなくrollbackとDB互換性のリスクになる。

このPackageでは、Package 1〜5で追加した保証をmainとReleaseで強制する。Web品質検査をCIへ追加し、成功したCIの対象SHAだけをRelease候補とし、全artifact成功後に初めてtagを作る。目的は「テストしたコード」と「配布したコード」と「tagが指すコード」を同一SHAへ固定することである。

### 10.2 対象ファイル候補

- `.github/workflows/ci.yml`
- `.github/workflows/release.yml`
- `web/package.json`
- 必要な場合のみ`web/tsconfig*.json`
- `AGENTS.md`
- `docs/deploy.md`

### 10.3 Web CI

`web/package.json`へ明示的なtypecheck scriptを追加する。

```json
"typecheck": "tsc --noEmit"
```

CIへNode 20とnpm cacheを追加し、次を必須化する。

```bash
npm --prefix web ci
npm --prefix web run typecheck
npm --prefix web test
npm --prefix web run build
```

Rust品質ゲートは削除・緩和しない。Webを別jobにして並列化してよいが、CI全体の成功条件に含める。

### 10.4 Release trigger

Release workflowは`main`へのpushを直接起点にせず、`CI` workflowの`workflow_run: completed`を起点にする。

要件：

- branchは`main`に限定する。
- `github.event.workflow_run.conclusion == 'success'`の場合だけrelease jobsを実行する。
- checkout、build、tag、releaseの対象SHAは`github.event.workflow_run.head_sha`で統一する。
- manual releaseが必要な場合はmain上でCIを`workflow_dispatch`し、その成功runからReleaseを起動する。Release単体の品質ゲート迂回用`workflow_dispatch`は残さない。
- `concurrency`を設定し、同一main releaseの競合を防ぐ。

### 10.5 Tag作成順序

現在のように最初にtagをpushしない。

新しい順序：

1. candidate tag名とversionを計算する。まだtagを作らない。
2. Web artifactをbuildする。
3. 全targetのRust release binaryをbuildする。
4. archiveとchecksumを生成する。
5. 全artifact job成功後にtagを作成・pushする。
6. 同じSHAとtagでGitHub Releaseを作成する。

buildへrelease tag文字列を埋め込む必要がある場合は、candidate tagをjob outputとして渡す。tag objectの存在をbuild依存にしない。

### 10.6 Release failure semantics

- CI失敗：Release jobを実行せずtagなし。
- Web build失敗：tagなし。
- いずれかのtarget build失敗：tagなし。
- tag作成後にGitHub Release作成だけが失敗：同じtagでrerun可能にする。別tagを増殖させない。
- 既存tag衝突時：勝手に別SHAへ同じtagを付け替えない。対象SHAが同じなら再利用し、異なるなら明示失敗する。

### 10.7 検証

- `actionlint`が利用可能なら両workflowへ実行する。
- PR上でRust jobとWeb jobが両方成功する。
- main merge後、CI成功より前にtagが作られていないことを確認する。
- Release runのcheckout SHA、artifact SHA、tag SHAが一致することを確認する。
- CI失敗runではRelease jobsがskipされ、tagが作られないことを確認する。

### 10.8 Package 6受入条件

- Web test／typecheck／buildがbranch protection対象CIに含まれる。
- CIまたはartifact buildが失敗したcommitへtagを作る経路がない。
- tag作成はartifact成功後である。
- `AGENTS.md`とdeploy文書のコマンド・Release説明が実際のworkflowと一致する。

---

## 11. 共通検証コマンド

各作業パッケージでは対象テストを先に実行する。Package単位のcommit前に、少なくとも変更対象のfmt、check、clippy、testを実行する。単一PRをReady for reviewへ変更する前に、以下をすべてclean environmentで実行する。

```bash
cargo fmt --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

npm --prefix web ci
npm --prefix web run typecheck
npm --prefix web test
npm --prefix web run build

git diff --check
```

依存関係またはworkflowを変更したPRでは追加で実行する。

```bash
cargo audit
cargo deny check
```

Web timezone回帰はPackage 1完了時と最終Phase検証で実行する。

```bash
TZ=UTC npm --prefix web test
TZ=Asia/Tokyo npm --prefix web test
```

コード規約確認：

```bash
rg '#\[allow\(dead_code\)\]' src web
```

既存箇所が検出された場合、今回の変更で追加していないことを確認する。新規追加は禁止する。変更対象に既存違反が含まれる場合は、不要コード削除またはversion検証など本来の用途へ接続して解消する。

---

## 12. 単一PRの実行手順

AIエージェントは次の手順を省略せず実行する。

### 12.1 初期化

1. `main`の最新状態とworking tree差分を確認する。
2. `fix/repository-stabilization-phase1`用Git Worktreeを1つだけ作る。既存差分をcheckoutで戻さない。
3. この計画と関連する仕様文書を読む。
4. Plan事前レビューを実施し、致命的指摘を反映する。
5. 必要ならDraft PRを1つ作り、6パッケージのチェックリストをPR descriptionへ記載する。
6. `.env`系ファイル、ローカル秘密設定、実tokenを読まない。テストには明示的なダミー値だけを使う。

### 12.2 各作業パッケージ

Package 1から6まで、順番に次を繰り返す。

1. 対象コードの実際の呼出関係を確認する。
2. 再現テストを先に追加し、意図した失敗を確認する。
3. production codeを実装する。
4. 対象テストを通す。
5. 関連文書の下書きを更新する。
6. `git diff`と`git diff --stat`を確認し、別Packageの変更が混入していないことを確認する。
7. 該当Packageの受入条件と自己レビューチェックリストを照合する。
8. Conventional Commitsで論理的にcommitする。
9. PR descriptionのPackageチェックリストと検証結果を更新する。
10. 失敗テストや未解決事項を残したまま次Packageへ進まない。

### 12.3 PR完成

1. 6パッケージ完了後、共通検証コマンドをclean environmentで全実行する。
2. commit列を先頭からレビューし、各commitがbuild可能かつ責務単位になっていることを確認する。
3. Phase 1最終受入試験を実施する。
4. 実装全体を本Planと項目単位で照合する。
5. Coderabbit／CodexでPR全体を自己レビューする。
6. 日本語のPR descriptionを完成させる。目的、変更、仕様判断、Package別差分、テスト、対象外、rollback方法を記載する。
7. Draftの場合はReady for reviewへ変更する。
8. リポジトリ規約に従いレビュー結果を待って対応する。レビュー対応commitも指摘対象のPackageへ対応づける。
9. 全レビュー対応後に共通検証を再実行する。
10. 未解決の指摘、失敗テスト、未実施検証がある状態で完了報告しない。

### Plan事前レビュー

実装開始前に、リポジトリへ置いた計画書を最大3回レビューする。

```bash
codex exec -m gpt-5.5 "このプランをレビューして。致命的な点だけ指摘して: docs/plan/plan-repository-stabilization-phase1.md"
```

更新後：

```bash
codex exec resume --last -m gpt-5.5 "プランを更新したからレビューして。致命的な点だけ指摘して: docs/plan/plan-repository-stabilization-phase1.md"
```

レビュー指摘を盲目的に採用しない。現行コードと仕様に照らして妥当性を確認する。

---

## 13. Package／PR自己レビューチェックリスト

各Packageのcommit前と、単一PRをReady for reviewへ変更する前の2回使用する。該当しない項目は理由をPRへ記載する。

### Correctness

- [ ] 問題を再現するテストを実装前に確認したか。
- [ ] happy pathだけでなくfailure pathを固定したか。
- [ ] fallbackによって不整合を隠していないか。
- [ ] error時にpartial stateが進まないか。
- [ ] 同じ入力が二重処理される新経路を作っていないか。

### Persistence

- [ ] DB更新前後の失敗挙動を確認したか。
- [ ] cursor／checkpoint／revisionの比較順序は決定的か。
- [ ] newer schemaを旧コードが処理していないか。
- [ ] Secret DBにも同じ保証が必要か確認したか。

### Async / Backpressure

- [ ] queue、channel、Mapに上限またはcleanupがあるか。
- [ ] lockをawait越しに保持していないか。
- [ ] taskの失敗がsilentになっていないか。
- [ ] rejectionを呼出元が成功と誤解しないか。

### Security

- [ ] secretをログ、error、metric labelへ含めていないか。
- [ ] scope解決失敗がNormalへ降格していないか。
- [ ] Redaction前の値を新しく永続化していないか。
- [ ] Security文書が実装以上の保証を主張していないか。

### Compatibility / Scope

- [ ] Phase 2／3の対象を先回りしていないか。
- [ ] 一時的な互換分岐を追加していないか。
- [ ] 公開APIを必要以上に増やしていないか。
- [ ] 変更対象外の挙動を変えていないか。

### Verification

- [ ] Rust fmt、check、clippy、test、docが成功したか。
- [ ] Web ci、typecheck、test、buildが成功したか。
- [ ] 関連文書を更新したか。
- [ ] 実装結果と本Planを項目単位で照合したか。
- [ ] 未実施項目を完了扱いにしていないか。

---

## 14. Phase 1最終受入試験

単一PRの全Package完了後、PR branchの最新commitからclean environmentで検証する。merge後はmainの同一commitでもRelease項目を確認する。

### 14.1 自動試験

- Rust全品質ゲート成功
- Web全品質ゲート成功
- UTC／Asia-Tokyo両方のWebテスト成功
- future normal DB rejection成功
- future secret DB rejection成功
- retryable LLM error時のcall count 1
- Tool後LLM error時のTool実行回数1
- 25セッションSleep drain成功
- hot session追加後も古いbacklog優先
- Telegram／Webhook token Redaction成功
- Webhook scope解決失敗時のenqueue 0
- per-session／global queue上限テスト成功
- tracker TTL／capacityテスト成功

### 14.2 統合確認

- default以外のAgentで作成したWeb chatへ継続送信し、同じAgentが応答する。
- 一時的なLLM 5xxを模擬し、入力とTool副作用が重複しない。
- queue fullを模擬し、Webhookが429を返す。
- Secret targetのWebhookがSecret DBを使用する。
- 不明なDiscord／Telegram targetが起動時またはrequest時に拒否される。
- main CI成功後にだけReleaseが開始される。
- tag SHAとartifact source SHAが一致する。

### 14.3 データ確認

- 既存DBをbackupしたコピーでmigrationを実行し、current schemaは正常に開く。
- future versionへ加工したコピーは内容を変更せず拒否される。
- Sleep後、処理対象sessionのcheckpointだけが進む。
- 失敗Sleepのcheckpointは進まない。

---

## 15. Phase 1完了条件

以下をすべて満たした時点でPhase 1を完了とする。

- [ ] 6つの作業パッケージが単一PRへ実装されている。
- [ ] 各Packageが論理的なcommitへ分離されている。
- [ ] 単一PRがmainへmerge済み。
- [ ] PRの未解決review threadがない。
- [ ] mainのRust／Web CIが成功している。
- [ ] Phase 1最終受入試験を実施済み。
- [ ] Release gateを実際のmain workflowで確認済み。
- [ ] 仕様文書が実装と一致している。
- [ ] Phase 2／3対象の先行実装が混入していない。
- [ ] 残課題をPhase 2 backlogへ明示的に引き継いでいる。
- [ ] この計画書を`docs/plan/archived/`へ移動している。

---

## 16. Phase 2への引継ぎ

Phase 1完了後も、次は未解決として残る。

- transient LLM errorに対する安全な自動再試行
- crash後のTurn途中再開
- Tool副作用のexactly-once相当制御
- 会話履歴の単一正本と単調増加sequence
- timestamp CASからinteger revisionへの移行
- Config snapshotの世代統一
- Sleep MemoryファイルとDBのクラッシュ整合性

Phase 2では、Phase 1で撤去したTurn全体リトライを戻すのではなく、`TurnId`、iteration境界、Tool実行台帳を前提に、安全な再試行を新しく設計する。
