# EgoPulse WebUI — Design System

WebUI 全体で共有するデザイントークンと、状態を持たない共通コンポーネントの仕様。

## 1. Color Palette

### 1.1 Surfaces

| 名称 | 色 | 用途 |
|---|---|---|
| `bg` | `#040812` | 最下層の背景。body 全体 |
| `panel` | `#081020` | Sidebar・Card・Modal の表面 |
| `panel-2` | `#0e1830` | panel 内の埋め込み要素（badge・code block 等の地面） |
| `panel-hover` | `#142244` | hover 可能な panel 系要素の hover 色 |

### 1.2 Text

| 名称 | 色 | 用途 |
|---|---|---|
| `text` | `#dce4f0` | 本文 |
| `text-strong` | `#f4f7fc` | 強調本文・見出し |
| `muted` | `#6b7fa8` | 補助情報・metadata |
| `muted-2` | `#4b5e80` | さらに弱い補助（timestamp 等） |

### 1.3 Accents

| 名称 | 色 | 用途 |
|---|---|---|
| `accent` | `#00d4ff`（シアン） | ブランドアクセント。primary action のアクセント |
| `accent-2` | `#c084fc`（パープル） | セカンドアクセント。ユーザー由来の強調 |

`accent` / `accent-2` は装飾用途に限る。テキストの着色には status 系（success / danger / warning）を使う。

### 1.4 Status

| 名称 | 色 | 用途 |
|---|---|---|
| `success` | `#5ceaff` | 成功・OK状態のテキスト |
| `danger` | `#f87171` | エラー・削除系アクションのテキスト |
| `warning` | `#fbbf24` | 警告・partial failure のテキスト |

各 status 色には対応する半透明背景（`*-soft`）を行ハイライト・バッジ背景・トースト背景に使う。

| 名称 | 色 |
|---|---|
| `accent-soft` | `rgba(0, 212, 255, 0.08)` |
| `accent-2-soft` | `rgba(192, 132, 252, 0.08)` |
| `danger-soft` | `rgba(248, 113, 113, 0.08)` |
| `success-soft` | `rgba(92, 234, 255, 0.08)` |
| `warning-soft` | `rgba(251, 191, 36, 0.08)` |

### 1.5 Borders

| 名称 | 色 | 用途 |
|---|---|---|
| `border` | `rgba(0, 212, 255, 0.10)` | 通常の枠線 |
| `border-strong` | `rgba(0, 212, 255, 0.22)` | active / focus 状態の枠線 |

---

## 2. Spacing

8px グリッドを基本とする。以下の段階的なスペーストークンを用意する：

| トークン | 値 | 主な用途 |
|---|---|---|
| 極小 | 4px | icon と text の隙間など |
| 小 | 8px | 関連要素間 |
| 中 | 12px | フォーム要素間 |
| 標準 | 16px | 標準的な gap |
| 中大 | 20px | 標準的な padding |
| 大 | 24px | section 間 |
| 特大 | 32px | 大セクション間 |
| 最大 | 40px | 主要レイアウト block 間 |

---

## 3. Radius

| サイズ | 値 | 用途 |
|---|---|---|
| 小 | 6px | badge・tag |
| 中 | 10px | input・select・code block |
| 大 | 14px | button・session item・カード一般 |
| 特大 | 20px | message bubble・tool card |
| 2特大 | 28px | modal・large card |
| full | 9999px | pill badge・status dot・avatar |

---

## 4. Typography

### 4.1 フォントファミリ

- 本文：システム-san-serif（`ui-sans-serif, system-ui, -apple-system, ...`）
- 等幅：システム-mono（`ui-monospace, SFMono-Regular, ...`）

Web フォントは読み込まない。OS 標準フォントで一貫した見え方を優先する。

### 4.2 サイズ階層

| 名称 | 用途 |
|---|---|
| `xs` | timestamp・badge 補助 |
| `sm` | 補助文・metadata |
| `base` | 本文（チャットメッセージ含む） |
| `md` | 強調本文 |
| `lg` | section 見出し |
| `xl` | 画面タイトル |

### 4.3 Weight

通常文は `400`、ラベル・見出しは `500` 〜 `700`。チャットメッセージの本文は `400` に固定し、strong / em 等のインライン強調のみ weight を上げる。

---

## 5. Elevation

| レベル | 用途 |
|---|---|
| flat | 通常の panel（Sidebar 等） |
| card | session item・tool card・sleep run card 等 |
| bubble | message bubble |
| modal | modal dialog・command palette |

シャドウは暗い背景に溶け込みすぎない程度に強め（`rgba(0,0,0,0.12)` 〜 `0.30`）。

---

## 6. Background Pattern

body の背景は、左上を中心とした放射状のアクセント色グラデーションと、上から下への暗いリニアグラデーションの2層構造とする。panel 系要素は半透明（`rgba(...)` 形式）にし、`backdrop-filter: blur(20px)` で背後のグラデーションを透かす。

---

## 7. Motion

### 7.1 Duration

| レベル | 時間 | 用途 |
|---|---|---|
| fast | 120ms | hover・focus |
| base | 160ms | button・item の状態変化 |
| slow | 240ms | modal・palette の開閉 |

アニメーションは状態遷移の補助に限定し、純装飾的な動きは加えない。

### 7.2 Reduced Motion

`prefers-reduced-motion: reduce` ではすべてのアニメーション・トランジションを実質即時（0.01ms）に切替る。ユーザーが OS で減らす設定をしている場合は必ず従う。

---

## 8. Z-Index

| レベル | 用途 |
|---|---|
| base | 通常コンテンツ |
| sticky | sticky header・chat status bar |
| sidebar | sidebar（mobile overlay 時） |
| toast | toast notification |
| modal | modal dialog |
| palette | command palette（modal より前面） |

---

## 9. 共通コンポーネントの振る舞い

### 9.1 Button

3 バリエーション + danger 系 + icon 系。

| バリエーション | 見た目 | 用途 |
|---|---|---|
| primary | `accent-2`（パープル）背景・濃色文字 | 主操作（送信・保存・実行） |
| secondary | `panel` 背景・本文色 | 副操作（キャンセル・閉じる） |
| icon | `panel` 背景・アイコンのみ | 小ボタン（refresh・close 等） |
| danger | `danger-soft` 背景・`danger` 文字 | 削除・破棄 |

状態：

- hover 時：わずかに上へ移動（1px）+ 明度アップ
- focus-visible 時：アクセント色の 2px outline
- disabled 時：opacity 0.5 + カーソル禁止 + hover 無効
- 処理中（busy）：左に小 spinner を挿入

### 9.2 Badge

channel 名・status・trigger_type 等の短いラベル表示用。高さは 20px 程度、小テキスト・uppercase・letter-spacing 強調。channel 系バッジは channel 名を統一の muted 表現で示す（channel 毎に色を変えない）。

### 9.3 Status Dot

agent の live 状態・runtime health の簡易表示に使う小円（8px 程度）。

| 色 | 意味 |
|---|---|
| live（accent + pulse アニメーション） | 実行中・アクティブ |
| idle（muted-2） | 待機中 |
| error（danger） | 直近でエラー |

### 9.4 Modal

汎用モーダルコンテナ。AuthModal・確認ダイアログ等はすべてこれを使う。

- backdrop は半透明の暗いオーバーレイ、click で閉じる
- ESC キーで閉じる
- panel 内はスクロール可能
- 開いたとき最初のインタラクティブ要素へフォーカス、閉じたとき呼び出し元へ復帰
- `role="dialog"` `aria-modal="true"` `aria-labelledby` 必須

### 9.5 Toast

画面右上に固定する通知。自動消去付き。

- 配置：Top Bar の下、画面右上
- 表示時間：info/success は4秒、error は8秒、warning は6秒
- 自動消去までの進行バーを下部に表示
- 手動で閉じるボタンを常時表示
- hover 中は自動消去タイマーを一時停止
- 最大4件まで表示・それ以上は破棄
- `role="status"`（info/success）または `role="alert"`（error/warning）

### 9.6 Empty State

リスト・履歴・実行結果が空のとき中央に表示。アイコン（24px）→ タイトル → 説明 →（必要に応じて）アクションボタン、を縦積み。

### 9.7 Spinner

- アクセント色・2px stroke の円形 spinner
- 3サイズ（小:12px / 中:16px / 大:24px）
- `role="status"` `aria-label="Loading"`

### 9.8 Card

Sleep run / Pulse run / Session 等のリスト項目で使う汎用カード。

- panel 背景・特大 radius・card レベルの shadow
- hover 時：枠線が強調色へ・わずかに上へ移動
- 選択中（active）：アクセント2色の枠線 + 内側に薄い ring

### 9.9 Code Block

メッセージ本文内・ツール結果等で使うコードブロック。

- 黒に近い半透明背景・中 radius・等幅フォント・小テキスト
- 横スクロール可（折り返さない）
- 右上に Copy ボタン（hover 時表示）。クリックでクリップボードへコピー、トーストで「Copied」を表示
- 言語指定がある場合は簡易シンタックスハイライトを適用

### 9.10 Section Title / Divider

- Section title：小テキスト・muted・uppercase・letter-spacing 強調
- Divider：薄い枠線色の水平線

---

## 10. アクセシビリティ

### 10.1 必須要件

- すべての操作可能要素は keyboard で到達可能（Tab / Shift+Tab）
- `:focus-visible` スタイルを必ず設定（アクセント色の 2px outline）
- Icon-only button は必ず `aria-label` を付ける
- Modal・Palette は focus trap を実装
- ライブ領域（チャット timeline・Metrics 数値）は `aria-live="polite"`

### 10.2 カラー コントラスト

- 本文 on bg：7:1 以上（WCAG AAA）
- muted on panel：4.5:1 以上（WCAG AA）
- status 色をテキストに使う場合は同様に AA 以上を確保

### 10.3 Reduced Motion

前述 §7.2 に従う。

---

## 11. アイコン戦略

SVG をコンポーネントとして内製する。外部 icon library は導入しない。

- 各アイコンは 24×24 viewBox・`stroke="currentColor"`・`stroke-width="2"`・`stroke-linecap="round"` を基本とする
- 必須アイコン種：hamburger・close・send・refresh・settings・sleep・pulse・metrics・command・agent・tool・copy・check・error・warning・lock・search

---

## 12. スタイル運用

- Tailwind CSS のユーティリティとカスタムクラスの混在を避ける。コンポーネント単位でカスタムクラスに集約する
- `bg-[rgba(...)]` のような inline arbitrary value は原則使わない
- やむを得ない一時的な調整はコメントで理由を明記
- 色・間隔・radius はすべてトークンを経由する
