#!/usr/bin/env python3
"""GitHub Issue を作成するスクリプト。

Usage:
    # 基本的な使い方
    python3 create_issue.py --title "[REQ] 機能名" --file requirements.md

    # ラベルを指定
    python3 create_issue.py --title "[REQ] 機能名" --file requirements.md \
      --category feature --component frontend

    # 複数コンポーネント
    python3 create_issue.py --title "[REQ] 機能名" --file requirements.md \
      --category fix --component backend --component frontend

    # 対話形式で作成
    python3 create_issue.py --interactive
"""

import argparse
import subprocess
import sys
from pathlib import Path

# ラベル例（参考）
CATEGORY_EXAMPLES = ["feature", "fix"]
COMPONENT_EXAMPLES = ["backend", "frontend", "ingest"]


def create_issue_with_gh(
    title: str, body: str, labels: list[str] | None = None
) -> None:
    """gh CLI を使って Issue を作成。

    Args:
        title: Issue のタイトル
        body: Issue の本文
        labels: ラベルのリスト（デフォルト: ["requirements"]）
    """
    if labels is None:
        labels = ["requirements"]

    cmd = ["gh", "issue", "create", "--title", title, "--body", body]

    for label in labels:
        cmd.extend(["--label", label])

    try:
        result = subprocess.run(cmd, check=True, capture_output=True, text=True)
        print(result.stdout)
        print("✓ Issue を作成しました")
    except subprocess.CalledProcessError as e:
        print(f"✗ Issue の作成に失敗しました: {e.stderr}", file=sys.stderr)
        sys.exit(1)
    except FileNotFoundError:
        print("✗ gh CLI がインストールされていません", file=sys.stderr)
        print("インストール方法: https://cli.github.com/", file=sys.stderr)
        sys.exit(1)


def read_file(file_path: Path) -> str:
    """ファイルから本文を読み込む。"""
    if not file_path.exists():
        print(f"✗ ファイルが見つかりません: {file_path}", file=sys.stderr)
        sys.exit(1)

    with file_path.open("r", encoding="utf-8") as f:
        return f.read()


def select_category() -> str | None:
    """カテゴリを選択（カスタム入力可）。"""
    print("\nカテゴリを選択してください:")
    for i, cat in enumerate(CATEGORY_EXAMPLES, 1):
        print(f"  {i}. {cat}")
    print("  0. スキップ")
    print("  または、カスタムラベルを直接入力")

    choice = input("選択 (1-2, 0, またはカスタム): ").strip()
    if choice == "0" or not choice:
        return None
    try:
        idx = int(choice) - 1
        if 0 <= idx < len(CATEGORY_EXAMPLES):
            return CATEGORY_EXAMPLES[idx]
    except ValueError:
        # カスタムラベルとして扱う
        return choice

    print("無効な選択です")
    return None


def select_components() -> list[str]:
    """コンポーネントを選択（複数可、カスタム入力可）。"""
    print("\nコンポーネントを選択してください（カンマ区切りで複数選択可）:")
    for i, comp in enumerate(COMPONENT_EXAMPLES, 1):
        print(f"  {i}. {comp}")
    print("  0. スキップ")
    print("  または、カスタムラベルを直接入力（カンマ区切り）")

    choice = input("選択 (例: 1,2 または custom1,custom2 または 0): ").strip()
    if choice == "0" or not choice:
        return []

    components = []
    for c in choice.split(","):
        c = c.strip()
        try:
            idx = int(c) - 1
            if 0 <= idx < len(COMPONENT_EXAMPLES):
                components.append(COMPONENT_EXAMPLES[idx])
        except ValueError:
            # カスタムラベルとして扱う
            if c:
                components.append(c)

    return components


def interactive_mode() -> None:
    """対話形式で Issue を作成。"""
    print("=== GitHub Issue 作成（対話モード）===\n")

    title = input("タイトル: ").strip()
    if not title:
        print("✗ タイトルは必須です", file=sys.stderr)
        sys.exit(1)

    print("\n本文を入力してください（EOF で終了: Ctrl+D）:")
    lines = []
    try:
        while True:
            line = input()
            lines.append(line)
    except EOFError:
        pass

    body = "\n".join(lines)

    # ラベル選択
    labels = ["requirements"]

    category = select_category()
    if category:
        labels.append(category)

    components = select_components()
    labels.extend(components)

    print("\n--- プレビュー ---")
    print(f"タイトル: {title}")
    print(f"ラベル: {', '.join(labels)}")
    print(f"\n{body}\n")

    confirm = input("この内容で Issue を作成しますか？ (y/N): ").strip().lower()
    if confirm != "y":
        print("キャンセルしました")
        sys.exit(0)

    create_issue_with_gh(title, body, labels)


def main() -> None:
    parser = argparse.ArgumentParser(
        description="GitHub Issue を作成するスクリプト",
        epilog=f"ラベル例:\n"
        f"  カテゴリ: {', '.join(CATEGORY_EXAMPLES)}\n"
        f"  コンポーネント: {', '.join(COMPONENT_EXAMPLES)}\n"
        f"(カスタムラベルも使用可能)",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--title", "-t", help="Issue のタイトル")
    parser.add_argument("--body", "-b", help="Issue の本文")
    parser.add_argument("--file", "-f", type=Path, help="本文を読み込むファイル")
    parser.add_argument(
        "--category",
        "-c",
        help=f"カテゴリラベル（例: {', '.join(CATEGORY_EXAMPLES)}）",
    )
    parser.add_argument(
        "--component",
        action="append",
        help=f"コンポーネントラベル（複数指定可、例: {', '.join(COMPONENT_EXAMPLES)}）",
    )
    parser.add_argument("--labels", "-l", help="追加ラベル（カンマ区切り）")
    parser.add_argument(
        "--interactive", "-i", action="store_true", help="対話形式で作成"
    )

    args = parser.parse_args()

    if args.interactive:
        interactive_mode()
        return

    if not args.title:
        parser.print_help()
        print("\n✗ --title は必須です", file=sys.stderr)
        sys.exit(1)

    body = ""
    if args.file:
        body = read_file(args.file)
    elif args.body:
        body = args.body
    else:
        parser.print_help()
        print("\n✗ --body または --file のいずれかは必須です", file=sys.stderr)
        sys.exit(1)

    # ラベルを構築
    labels = ["requirements"]

    if args.category:
        labels.append(args.category)

    if args.component:
        labels.extend(args.component)

    if args.labels:
        labels.extend([label.strip() for label in args.labels.split(",")])

    create_issue_with_gh(args.title, body, labels)


if __name__ == "__main__":
    main()
