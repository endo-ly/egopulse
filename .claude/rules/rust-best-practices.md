---
paths:
  - "**/*.rs"
  - "**/Cargo.toml"
  - Cargo.toml
---

# Rust Best Practices Guide (2026.04)

本ドキュメントは、Rust プロジェクト全般で使える実践的なベストプラクティス集です。単なるコーディング規約ではなく、Rust がなぜその設計を推奨するのか、どこで品質差が生まれやすいのかまで含めて整理しています。

Rust は「速い言語」である前に、「所有権と型によってバグの余地を前倒しで潰す言語」です。したがって、よい Rust コードとは、コンパイルを通すコードではなく、型・所有権・境界設計によって誤用しにくく、変更しやすく、障害時にも壊れ方が読みやすいコードです。

---

## 1. 設計思想

### 1.1 Rust で最も大事なこと

Rust では、実行時に気をつけるより、コンパイル時に危険を表現しきる方が強い設計です。  
そのため、良い Rust コードは「利用者が注意深く使えば安全」ではなく、「雑に使っても壊れにくい」方向を目指します。

この思想から、以下を優先します。

- 制約をコメントではなく型で表す
- 運用ルールではなく API 形状で誤用を防ぐ
- 便利な抜け道より、正しい使い方が自然に見える設計を選ぶ

### 1.2 判断の優先順位

Rust では性能最適化の余地が比較的大きいため、先に読みやすく正しい構造を作る方が長期的に有利です。判断が競合したら、原則として次の順で優先します。

1. 正しさ
2. 安全性
3. 保守性
4. 可読性
5. 性能
6. 記述量の少なさ

### 1.3 基本原則

- safe Rust で書けるなら safe Rust を選ぶ
- 型で表現できる制約は型で表現する
- 一時的に楽な設計より、変更に強い設計を選ぶ
- 標準ライブラリや Rust API Guidelines の慣習を優先する

---

## 2. ツールチェーンとプロジェクト設定

### 2.1 なぜ設定が重要か

Rust はエコシステムの進化が速く、edition・MSRV・resolver の差が設計や依存解決に直接影響します。  
設定を明示しないと、「誰の環境ならビルドできるのか」「どの機能を前提にしてよいのか」が曖昧になり、保守コストが急激に上がります。

### 2.2 必須ルール

- `edition` を明示する
- `rust-version` を明示する
- stable toolchain を前提にする
- 整形基準は `rustfmt` に統一する

### 2.3 推奨ルール

- workspace を使う場合は共有設定を root に寄せる
- virtual workspace では `resolver` を明示する
- MSRV 方針は依存更新方針とセットで管理する

### 2.4 避けること

- ツールチェーン要件を暗黙にすること
- nightly 前提で主要設計を組むこと
- crate ごとに依存や lint の方針が分裂すること

---

## 3. モジュール設計

### 3.1 Rust における良い分割とは

Rust は borrow checker と可視性制御が強いため、責務の境界が明確なほど設計の強さが増します。逆に、巨大モジュールに責務を押し込むと、所有権の流れ、エラー境界、非同期境界が混ざり、局所修正が難しくなります。

よいモジュールは、「何を担当するか」が一文で説明でき、内部実装を知らなくても外から使えます。

### 3.2 分ける単位

責務は少なくとも次の観点で分離を検討します。

- ドメインロジック
- I/O
- 変換
- 設定
- エラー定義
- 非同期 orchestration

### 3.3 良い構造の条件

- `pub` が最小限で済む
- テスト対象が明確
- I/O なしでもドメインロジックを検証できる
- 非同期制御を外してもコアロジックが読める

### 3.4 避けること

- 巨大モジュール
- 永続化と業務ロジックの密結合
- 1 関数に「取得・検証・保存・通知・表示」を詰め込むこと

---

## 4. API 設計

### 4.1 なぜ API 設計が Rust では重要か

Rust は型システムが強いため、API の形そのものが仕様になります。  
呼び出し側はシグネチャを見て設計意図を読み取るので、API が曖昧だと、実装が正しくても使われ方が不安定になります。

### 4.2 良い API の条件

- 名前から責務が分かる
- 誤用しにくい
- 呼び出し側に内部事情を漏らさない
- 例外的ケースが型に現れている

### 4.3 関数とメソッド

操作対象が明確なら method を優先します。Rust では method の方が発見しやすく、autoborrow も効き、利用者が「この型に何ができるか」を把握しやすいからです。

- 明確な受け手がある操作は method にする
- 単独の変換や集約処理は free function でもよい
- `new` は最も基本的な constructor に使う
- 意味のある生成には `open`, `connect`, `parse`, `load` などを使う

### 4.4 引数設計

Rust では、引数の意味が曖昧だと、利用者はコメントや実装を読まなければならなくなります。これは型で安全を取れる言語の強みを捨てることになります。

- `bool` フラグ引数は原則避ける
- `Option<T>` をモード切替の代用品にしない
- 意味のある組は dedicated struct にまとめる
- 位置引数だけでは意味が伝わらないなら型を分ける

### 4.5 戻り値設計

- out-parameter ではなく return value を使う
- 複数値を返すときは tuple より名前付き struct を優先する
- 失敗し得るなら `Result`
- 値の不在が正常系なら `Option`

---

## 5. 型設計

### 5.1 Rust らしい設計の中心

Rust の強さは、値の意味を型に押し込めることです。  
同じ `String` でも、それが user id なのか URL なのか token なのか設定値なのかで制約は異なります。ここを primitive のまま流すと、コンパイラは何も守ってくれません。

### 5.2 型で表現すべきもの

- 単位
- ID
- 状態
- モード
- 妥当性検証済みの値
- 他と混同してはいけない値

### 5.3 推奨パターン

- newtype
- enum
- dedicated struct
- smart constructor

これらは「過剰設計」ではなく、Rust の強みを使う最短経路です。

### 5.4 trait 実装方針

Rust では trait 実装が型の使い勝手を大きく左右します。`Display` や `Debug` がないだけで調査性が落ち、`Eq` や `Hash` がないだけで API 利用の幅が狭くなります。

妥当なら次を早めに検討します。

- `Debug`
- `Display`
- `Clone`
- `Eq` / `PartialEq`
- `Ord` / `PartialOrd`
- `Hash`
- `Default`

### 5.5 変換規約

変換は Rust 標準の慣習に従うべきです。独自流儀を入れると、利用者は crate ごとに学習し直す必要が出ます。

- `From`
- `TryFrom`
- `AsRef`
- `AsMut`

を優先します。

- `Into` / `TryInto` は独自実装しない
- 高コスト・失敗可能・情報落ちの変換は、それが分かる API にする

### 5.6 避けること

- `String` に異なる意味の値を詰め込むこと
- `HashMap<String, Value>` をドメインモデル代わりに使うこと
- smart pointer でもない型へ `Deref` / `DerefMut` を安易に実装すること

---

## 6. エラー処理

### 6.1 Rust におけるエラー処理の考え方

Rust は例外ではなく `Result` を基本にするため、エラー設計は制御フロー設計そのものです。  
ここを雑にすると、「どこで失敗するのか」「何が回復可能なのか」「何を利用者が握るべきか」が曖昧になります。

### 6.2 基本方針

- 回復可能な失敗は `Result` で返す
- `panic!` はバグか回復不能状態に限定する
- エラーは文字列ではなく型として設計する

### 6.3 エラー型の要件

- `std::error::Error` を実装する
- 可能なら `Send + Sync + 'static` を満たす
- `Display` は簡潔で lower-case を基本とする
- `()` をエラー型に使わない

### 6.4 実装ルール

- 境界で文脈を追加し、原因は保持する
- 構造化エラー定義には `thiserror` を優先する
- 利用者向け文言と内部原因を必要に応じて分ける

### 6.5 `panic!` と `unwrap`

`panic!` は「あり得る失敗」の処理ではなく、「設計が壊れている」ことの表明です。  
そのため、本番コードでの `unwrap()` は、バグをランタイムまで先送りする行為になりやすいです。

- 本番コードでの `unwrap()` は原則禁止
- `expect()` は、失敗しない理由を説明できる場合のみ許可
- `panic!` は不変条件違反、到達不可能分岐、継続不能状態に限定する

---

## 7. 所有権・借用・ライフタイム

### 7.1 Rust ならではの最重要ポイント

Rust を他言語と分ける本質は、所有権によってメモリ安全と並行安全の多くをコンパイル時に保証することです。  
したがって、Rust で設計がうまいとは、borrow checker をねじ伏せることではなく、所有権の流れが自然に見える設計にすることです。

### 7.2 基本姿勢

- borrow を必要以上に伸ばさない
- 所有権の移動を設計手段として使う
- clone は必要性が明確なときだけ行う

### 7.3 実践ルール

- ライフタイム注釈で押し切る前に所有モデルを見直す
- 関数境界で borrow 関係を簡潔に保つ
- 共有可変状態より ownership transfer を優先する

### 7.4 避けること

- clone による場当たり的な borrow 回避
- 長く生きる参照を API に持ち込みすぎること
- 内部都合で利用者に複雑な lifetime を背負わせること

---

## 8. 非同期処理と並行性

### 8.1 Rust の async が難しい理由

Rust の async は「軽いスレッド」ではなく、「状態機械化された future」です。  
そのため、同期コードの直感のまま書くと、blocking、cancel、shutdown、shared state で問題が起きやすくなります。

### 8.2 async 設計の原則

- async 文脈で blocking 処理を直接実行しない
- CPU-bound / blocking I/O は専用手段へ分離する
- 長寿命 task は lifecycle を明示する

### 8.3 task 管理

`spawn` は便利ですが、タスクの寿命管理を曖昧にしやすいです。  
Rust ではメモリ安全は守れても、タスクの孤児化や shutdown 漏れは普通に起こります。

- `spawn` を「とりあえず並列化」の道具にしない
- `JoinHandle` を捨てるときは理由が明確であること
- キャンセル時の整合性を最初から設計する

### 8.4 共有状態

- まず message passing を検討する
- `Arc<Mutex<T>>` / `Arc<RwLock<T>>` は必要最小限にする
- lock を保持したまま `.await` しない
- async 境界で guard を生かし続けない

### 8.5 graceful shutdown

shutdown は「終了シグナルを送る」だけでは不十分です。  
実際には、停止通知、停止処理、終了待機の 3 段階を設計しないと、タスクリークや中途半端な終了が起こります。

- 停止通知は `CancellationToken` 系を優先する
- 停止処理は idempotent にする
- 終了待機は join / tracker / close 手段を明示的に持つ
- `abort` は最終手段とみなす

### 8.6 retry と timeout

- retry には停止条件を持たせる
- backoff 戦略を持たせる
- timeout 時の後始末を設計に含める
- 期待された再試行を何でも `error` にしない

---

## 9. `unsafe` と低レベルコード

### 9.1 なぜ `unsafe` が特別なのか

`unsafe` は「危険なコード」ではなく、「コンパイラの保証の一部を自分で引き受ける宣言」です。  
つまり `unsafe` を書いた時点で、その周囲の正しさはレビューと設計で守るしかありません。

### 9.2 基本原則

- `unsafe` は最後の手段
- `unsafe` は狭い範囲に閉じ込める
- 外側には安全な API を提供する

### 9.3 必須ルール

- `unsafe fn` / `unsafe` block には不変条件を書く
- なぜ safe Rust では不十分か説明できること
- `Send` / `Sync` の手動実装は極めて慎重に扱う
- raw pointer を扱う型は thread safety を検証する

### 9.4 禁止事項

- 推測だけで `unsafe` を入れること
- `unsafe` の呼び出し点をコードベース全体に散らすこと

---

## 10. パフォーマンス

### 10.1 Rust の性能設計で大事なこと

Rust は抽象化コストをかなり消してくれるので、最初から low-level に寄せすぎる必要はありません。  
むしろ、可読性を壊した最適化は、将来のバグ修正や設計変更を難しくします。

### 10.2 基本原則

性能改善は次の順で行います。

1. 正しさを確保する
2. profile / benchmark で測定する
3. ボトルネックを特定する
4. 最小限の変更で改善する

### 10.3 実践ルール

- iterator や抽象化を早々に疑わない
- hot path で clone・割り当て・文字列化を抑える
- CPU-bound と I/O-bound を分けて考える
- キャッシュ導入前に寿命・無効化・整合性コストを設計する

### 10.4 避けること

- benchmark なしの micro-optimization
- 可読性を大きく落とす手書き最適化
- async 文脈での同期 blocking

---

## 11. ドキュメント

### 11.1 Rust で doc が重要な理由

Rust では public API の意味が型に強く現れますが、それでも「失敗条件」「panic 条件」「安全条件」は型だけでは伝えきれません。  
そのため rustdoc は補足資料ではなく、API 契約の一部として扱うべきです。

### 11.2 rustdoc 方針

- crate-level docs を書く
- public item に doc comment を付ける
- public API には例を書く
- fallible API には `# Errors`
- panic し得る API には `# Panics`
- `unsafe` API には `# Safety`

### 11.3 良い例コード

- `unwrap()` より `?` を優先する
- 「どう使うか」だけでなく「なぜ使うか」を示す
- doctest が通る状態を保つ

---

## 12. テスト

### 12.1 Rust におけるテストの位置づけ

Rust は型で多くを守れますが、仕様までは証明してくれません。  
とくに状態遷移、境界条件、非同期連携、I/O 失敗時の振る舞いはテストが必要です。

### 12.2 テスト戦略

- 単体テストはロジックを検証する
- 統合テストは境界をまたぐ振る舞いを検証する
- 実装詳細ではなく観測可能な結果を検証する

### 12.3 単体テストで重視するもの

- parser
- validation
- state transition
- error mapping
- pure function

### 12.4 統合テストで重視するもの

- ネットワーク
- ファイル I/O
- DB
- 外部プロセス
- 非同期連携

### 12.5 非同期テストの注意

- timeout を明示する
- 背景 task を取りこぼさない
- shutdown 完了を待つ
- sleep ベースの flaky な検証を最小化する

---

## 13. ロギングと可観測性

### 13.1 なぜ可観測性が必要か

Rust はメモリ破壊の類は減らせますが、外部 I/O、分散障害、timeout、競合、依存先不調までは防げません。  
本番で何が起きたかを再構築できるようにするには、ログ設計が必要です。

### 13.2 原則

- `println!` ではなく `tracing` を使う
- ログは障害解析に使える粒度で出す
- 秘密情報を出さない

### 13.3 実践ルール

- structured logging を優先する
- retry、timeout、fallback、shutdown、I/O failure を観測可能にする
- 相関に必要な ID や状態は field として残す
- 期待された再試行を何でも `error` にしない

---

## 14. rustfmt / Clippy / CI

### 14.1 rustfmt

Rust では formatting を議論対象にしない方が得です。  
`rustfmt` に寄せることで、レビューは意味のある差分に集中できます。

- `cargo fmt` を唯一の整形基準にする
- style の個別流儀は持ち込まない

### 14.2 Clippy

Clippy は Rust の「慣習とのズレ」を早めに見つけるための重要な補助線です。ただし、lint を増やせばよいわけではなく、false positive コストも管理する必要があります。

- `cargo clippy --all-targets --all-features -- -D warnings` を基本にする
- `clippy::pedantic` は全有効化せず、必要な lint を選んで採用する
- `clippy::restriction` の全有効化はしない
- `#[allow]` を置くときは理由を説明できる状態にする

### 14.3 CI

- format
- lint
- test
- doc

を継続的に実行する。

---

## 15. レビュー観点

### 15.1 設計

- 型で不変条件を表現できているか
- API 名と責務が一致しているか
- モジュール境界が明確か

### 15.2 実装

- clone が場当たり的でないか
- `.await` と lock の境界が安全か
- `panic!` が回復可能エラーを潰していないか
- `unsafe` の範囲が最小か

### 15.3 品質保証

- テストが振る舞いを検証しているか
- doc が利用者視点になっているか
- ログが本番解析に使えるか

---

## 16. 参考ソース

### Rust 公式

- Rust Style Guide  
  https://doc.rust-lang.org/style-guide/index.html
- Rust Edition Guide: Rust 2024  
  https://doc.rust-lang.org/edition-guide/rust-2024/index.html
- The Rust Programming Language: To panic! or Not to panic!  
  https://doc.rust-lang.org/book/ch09-03-to-panic-or-not-to-panic.html
- The Cargo Book: Workspaces  
  https://doc.rust-lang.org/cargo/reference/workspaces.html
- The Cargo Book: Rust Version  
  https://doc.rust-lang.org/cargo/reference/rust-version.html
- The Cargo Book: Features  
  https://doc.rust-lang.org/cargo/reference/features.html
- The Cargo Book: SemVer Compatibility  
  https://doc.rust-lang.org/cargo/reference/semver.html
- The rustdoc book: Lints  
  https://doc.rust-lang.org/rustdoc/lints.html
- Clippy Documentation  
  https://doc.rust-lang.org/nightly/clippy/lints.html

### Rust エコシステム

- Rust API Guidelines  
  https://rust-lang.github.io/api-guidelines/
- Rust API Guidelines: Documentation  
  https://rust-lang.github.io/api-guidelines/documentation.html
- Rust API Guidelines: Interoperability  
  https://rust-lang.github.io/api-guidelines/interoperability.html
- Rust API Guidelines: Type safety  
  https://rust-lang.github.io/api-guidelines/type-safety.html
- Rust API Guidelines: Predictability  
  https://rust-lang.github.io/api-guidelines/predictability.html
- Rust API Guidelines: Dependability  
  https://rust-lang.github.io/api-guidelines/dependability.html

### 非同期ランタイム

- Tokio: Graceful Shutdown  
  https://tokio.rs/tokio/topics/shutdown
- Tokio task module docs  
  https://docs.rs/tokio/latest/tokio/task/index.html
