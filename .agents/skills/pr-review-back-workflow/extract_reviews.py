#!/usr/bin/env python3
"""
レビューコメント抽出スクリプト

Usage:
    python3 extract_reviews.py <PR_NUMBER> [--truncate N]

デフォルトでコメント全文を表示します。--truncate N で N 文字に切り詰めます。
Resolved状態のコメントは除外されます。
"""

import argparse
import json
import subprocess
import sys


def run_gh_command(args: list[str]) -> dict | list | None:
    """gh コマンドを実行してJSON結果を返す"""
    try:
        result = subprocess.run(
            ["gh"] + args, capture_output=True, text=True, check=True
        )
        return json.loads(result.stdout)
    except subprocess.CalledProcessError as e:
        print(f"Error running gh command: {e.stderr}", file=sys.stderr)
        return None
    except json.JSONDecodeError as e:
        print(f"Error parsing JSON: {e}", file=sys.stderr)
        return None


def truncate_text(text: str, max_length: int | None) -> str:
    """テキストを指定長で切り詰める（Noneで切り詰めなし）"""
    if max_length is None or len(text) <= max_length:
        return text
    return text[:max_length] + "..."


def run_graphql_query(query: str) -> dict | None:
    """GitHub GraphQL APIクエリを実行"""
    try:
        result = subprocess.run(
            ["gh", "api", "graphql", "-f", f"query={query}"],
            capture_output=True,
            text=True,
            check=True,
        )
        return json.loads(result.stdout)
    except (subprocess.CalledProcessError, json.JSONDecodeError) as e:
        print(f"GraphQL query failed: {e}", file=sys.stderr)
        return None


def get_resolved_comment_ids(owner: str, repo: str, pr_number: str) -> set[int]:
    """GraphQL APIを使ってResolvedなコメントのIDセットを取得"""
    query = f"""
    {{
      repository(owner: "{owner}", name: "{repo}") {{
        pullRequest(number: {pr_number}) {{
          reviewThreads(first: 100) {{
            nodes {{
              isResolved
              comments(first: 100) {{
                nodes {{
                  databaseId
                }}
              }}
            }}
          }}
        }}
      }}
    }}
    """

    result = run_graphql_query(query)
    if not result:
        return set()

    resolved_ids = set()
    try:
        threads = result["data"]["repository"]["pullRequest"]["reviewThreads"]["nodes"]
        for thread in threads:
            if thread["isResolved"]:
                for comment in thread["comments"]["nodes"]:
                    if comment.get("databaseId"):
                        resolved_ids.add(comment["databaseId"])
    except (KeyError, TypeError) as e:
        print(f"Failed to parse GraphQL response: {e}", file=sys.stderr)

    return resolved_ids


def main():
    parser = argparse.ArgumentParser(
        description="レビューコメント抽出スクリプト"
    )
    parser.add_argument("pr_number", help="PR番号")
    parser.add_argument(
        "--truncate", type=int, default=None, metavar="N",
        help="コメント本文をN文字に切り詰める（デフォルト: 全文表示）",
    )
    args = parser.parse_args()

    pr_number = args.pr_number
    max_length = args.truncate

    print(f"Fetching data for PR #{pr_number}...", file=sys.stderr)

    # リポジトリ情報の取得
    repo_info = run_gh_command(["repo", "view", "--json", "owner,name"])
    if not repo_info:
        sys.exit(1)

    owner = repo_info["owner"]["login"]
    repo = repo_info["name"]

    # Resolved状態のコメントIDを取得
    print("  Fetching resolved comment IDs...", file=sys.stderr)
    resolved_ids = get_resolved_comment_ids(owner, repo, pr_number)
    if resolved_ids:
        print(
            f"  Found {len(resolved_ids)} resolved comments (will be excluded)",
            file=sys.stderr,
        )

    # 1. Inline Comments (コード行への指摘)
    print("  Fetching review comments...", file=sys.stderr)
    review_comments = (
        run_gh_command(["api", f"repos/{owner}/{repo}/pulls/{pr_number}/comments"])
        or []
    )

    # 2. Issue Comments (全体コメント、要約など)
    print("  Fetching issue comments...", file=sys.stderr)
    issue_comments = (
        run_gh_command(["api", f"repos/{owner}/{repo}/issues/{pr_number}/comments"])
        or []
    )

    print(f"\n# Review Report (PR #{pr_number})\n")

    # --- Inline Comments ---
    coderabbit_inline = [
        c
        for c in review_comments
        if "coderabbitai" in c["user"]["login"].lower()
        and c.get("id") not in resolved_ids  # Resolvedを除外
    ]

    if coderabbit_inline:
        print("## 🚨 Code Suggestions (Inline)\n")
        for c in coderabbit_inline:
            path = c.get("path", "unknown")
            line = c.get("line") or c.get("original_line") or "?"
            body = c.get("body", "").replace("\n", " ")
            url = c.get("html_url", "")

            # コメント全体を表示（max_lengthまで）
            summary = truncate_text(body, max_length)

            print(f"- [ ] **{path}:{line}**")
            print(f"  - 指摘: {summary}")
            print(f"  - [View on GitHub]({url})\n")
    else:
        print(
            "## 🚨 Code Suggestions (Inline)\n\nNo unresolved inline comments found.\n"
        )

    # --- Summary / Walkthrough ---
    coderabbit_general = [
        c for c in issue_comments if "coderabbitai" in c["user"]["login"].lower()
    ]

    if coderabbit_general:
        print("## 📝 Summary & Walkthrough\n")
        for c in coderabbit_general:
            body = c.get("body", "")
            url = c.get("html_url", "")

            # Walkthroughなどの長文コメントはリンクのみ
            if "Walkthrough" in body or "Summary" in body:
                print(f"- [ ] **PR Summary / Report** ([View on GitHub]({url}))")
            else:
                # 短いコメントなら表示
                summary = truncate_text(body, 80)
                print(f"- [ ] **Comment**: {summary} ([Link]({url}))")

    print("\n---")
    print("Generated checklist above. Review and check off items as you address them.")

    # 統計情報
    total_inline = len(
        [
            c
            for c in review_comments
            if "coderabbitai" in c.get("user", {}).get("login", "").lower()
        ]
    )
    unresolved_inline = len(coderabbit_inline)
    stats_msg = (
        f"\n📊 Stats: {unresolved_inline} unresolved / "
        f"{total_inline} total inline comments"
    )
    print(stats_msg, file=sys.stderr)


if __name__ == "__main__":
    main()
