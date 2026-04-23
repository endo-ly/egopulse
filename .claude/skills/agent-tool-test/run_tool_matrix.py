#!/usr/bin/env python3
"""全モデル x 全ツールの実行テストを行う。

使い方:
    uv run python .claude/skills/agent-tool-test/run_tool_matrix.py \
      --api-url http://127.0.0.1:8000 \
      --models glm-4.7,xiaomi/mimo-v2-flash:free \
      --start-date 2026-01-01 --end-date 2026-01-31 \
      --limit 5 --granularity day

モデル指定が無い場合は全モデルが対象になります。
ツールはAPIから動的に取得します。
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from dataclasses import dataclass
from datetime import date
from typing import Any

import httpx

DEFAULT_API_URL = "http://127.0.0.1:8000"


@dataclass(frozen=True)
class ToolCase:
    name: str
    prompt: str


def _build_tool_cases(
    tools: list[dict[str, Any]],
    start_date: str,
    end_date: str,
    limit: int,
    granularity: str,
) -> list[ToolCase]:
    """APIから取得したツール情報からテストケースを構築します。

    Args:
        tools: ツール情報のリスト(APIから取得)
        start_date: 開始日
        end_date: 終了日
        limit: リミット
        granularity: 粒度

    Returns:
        テストケースのリスト
    """
    tool_cases: list[ToolCase] = []

    for tool in tools:
        tool_name = tool["name"]
        description = tool.get("description", "")

        # ツール名に応じたプロンプトを構築
        if "watch_history" in tool_name:
            prompt = (
                f"Call {tool_name} with start_date={start_date}, "
                f"end_date={end_date}, limit={limit}. "
                f"Return only tool calls. Tool description: {description}"
            )
        elif "watching_stats" in tool_name or "listening_stats" in tool_name:
            prompt = (
                f"Call {tool_name} with start_date={start_date}, "
                f"end_date={end_date}, granularity={granularity}. "
                f"Return only tool calls. Tool description: {description}"
            )
        elif "top_tracks" in tool_name or "top_channels" in tool_name:
            prompt = (
                f"Call {tool_name} with start_date={start_date}, "
                f"end_date={end_date}, limit={limit}. "
                f"Return only tool calls. Tool description: {description}"
            )
        elif "activity_stats" in tool_name:
            prompt = (
                f"Call {tool_name} with start_date={start_date}, "
                f"end_date={end_date}, granularity={granularity}. "
                f"Return only tool calls. Tool description: {description}"
            )
        elif "repositories" in tool_name:
            # repositoriesは日付パラメータが不要
            prompt = (
                f"Call {tool_name} to get repository list. "
                f"Return only tool calls. Tool description: {description}"
            )
        else:
            # デフォルト: ツール名のみを指定
            prompt = (
                f"Call {tool_name} with appropriate parameters. "
                f"Return only tool calls. Tool description: {description}"
            )

        tool_cases.append(ToolCase(name=tool_name, prompt=prompt))

    return tool_cases


def _iter_sse_events(response: httpx.Response):
    event_name: str | None = None
    for line in response.iter_lines():
        if not line:
            event_name = None
            continue
        if line.startswith("event:"):
            event_name = line.split(":", 1)[1].strip()
            continue
        if line.startswith("data:"):
            payload = line.split(":", 1)[1].strip()
            yield event_name or "message", payload


def _stream_tool_call(
    client: httpx.Client,
    url: str,
    headers: dict[str, str],
    model_name: str,
    prompt: str,
    tool_name: str,
    timeout: float,
) -> dict[str, Any]:
    payload = {
        "model_name": model_name,
        "stream": True,
        "messages": [
            {
                "role": "user",
                "content": (
                    "You are a tool runner. "
                    "You MUST call the specified tool exactly once and nothing else. "
                    f"Tool: {tool_name}. {prompt}"
                ),
            }
        ],
    }

    result: dict[str, Any] = {
        "model": model_name,
        "tool": tool_name,
        "llm_response": None,
        "events": [],
        "error": None,
    }

    try:
        with client.stream(
            "POST", url, json=payload, headers=headers, timeout=timeout
        ) as resp:
            if resp.status_code != 200:
                result["llm_response"] = f"HTTP {resp.status_code}: {resp.text}"
                return result

            for event, data in _iter_sse_events(resp):
                if not data:
                    continue
                try:
                    chunk = json.loads(data)
                except json.JSONDecodeError:
                    continue

                result["events"].append({"event": event, "data": chunk})

                if chunk.get("delta"):
                    if not result["llm_response"]:
                        result["llm_response"] = ""
                    result["llm_response"] += chunk["delta"]

            return result
    except httpx.RequestError as exc:
        result["error"] = f"HTTP error: {exc}"
        return result


def _get_models(client: httpx.Client, url: str, headers: dict[str, str]) -> list[str]:
    resp = client.get(url, headers=headers, timeout=30.0)
    resp.raise_for_status()
    data = resp.json()
    return [model["id"] for model in data.get("models", [])]


def _get_tools(
    client: httpx.Client, api_url: str, headers: dict[str, str]
) -> list[dict[str, Any]]:
    """APIからツール一覧を取得します。

    Args:
        client: httpx クライアント
        api_url: API URL
        headers: リクエストヘッダー

    Returns:
        ツール情報のリスト
    """
    try:
        resp = client.get(f"{api_url}/v1/chat/tools", headers=headers, timeout=30.0)
        resp.raise_for_status()
        data = resp.json()
        return data.get("tools", [])
    except httpx.HTTPError as exc:
        print(f"Failed to fetch tools: {exc}", file=sys.stderr)
        return []


def _parse_args() -> argparse.Namespace:
    today = date.today().strftime("%Y-%m-%d")
    parser = argparse.ArgumentParser(description="Run tool matrix test.")
    parser.add_argument("--api-url", default=DEFAULT_API_URL)
    parser.add_argument("--api-key", default=os.getenv("API_KEY"))
    parser.add_argument(
        "--api-key-header",
        default=os.getenv("API_KEY_HEADER", "Authorization"),
    )
    parser.add_argument(
        "--models",
        default=None,
        help="Comma-separated model list (default: all models)",
    )
    parser.add_argument(
        "--tools",
        default=None,
        help="Comma-separated tool list (default: all tools)",
    )
    parser.add_argument("--start-date", default="2026-01-01")
    parser.add_argument("--end-date", default="2026-01-31")
    parser.add_argument("--limit", type=int, default=5)
    parser.add_argument("--granularity", default="day")
    parser.add_argument("--timeout", type=float, default=120.0)
    parser.add_argument("--output", default=f"tool_matrix_results_{today}.json")
    return parser.parse_args()


def main() -> int:
    args = _parse_args()
    api_url = args.api_url.rstrip("/")
    headers: dict[str, str] = {}
    if args.api_key:
        headers[args.api_key_header] = args.api_key

    with httpx.Client() as client:
        # モデルリストを取得
        if args.models:
            # 引数で指定されたモデルを使用
            models = [m.strip() for m in args.models.split(",")]
        else:
            # 全モデルを取得
            try:
                models = _get_models(client, f"{api_url}/v1/chat/models", headers)
            except httpx.HTTPError as exc:
                print(f"Failed to fetch models: {exc}", file=sys.stderr)
                return 1

        # ツールリストをAPIから取得
        tools = _get_tools(client, api_url, headers)
        if not tools:
            print("No tools available for testing.", file=sys.stderr)
            return 1

        # --toolsオプションで指定されたツールのみを対象にする
        if args.tools:
            specified_tools = [t.strip() for t in args.tools.split(",")]
            tools = [t for t in tools if t["name"] in specified_tools]
            if not tools:
                print(f"None of the specified tools found: {specified_tools}", file=sys.stderr)
                return 1

        print(f"Found {len(tools)} tools: {[t['name'] for t in tools]}")
        print(f"Testing {len(models)} models: {models}")
        print()

        # ツールケースを構築
        tool_cases = _build_tool_cases(
            tools,
            args.start_date,
            args.end_date,
            args.limit,
            args.granularity,
        )
        results: list[dict[str, Any]] = []

        for model in models:
            for tool_case in tool_cases:
                print(f"Testing: {model} - {tool_case.name}")
                result = _stream_tool_call(
                    client,
                    f"{api_url}/v1/chat",
                    headers,
                    model,
                    tool_case.prompt,
                    tool_case.name,
                    args.timeout,
                )
                results.append(result)

                # LLM応答を表示
                if result["llm_response"]:
                    print(f"  Response: {result['llm_response'][:200]}")
                else:
                    print("  Response: (empty)")
                print()

        with open(args.output, "w", encoding="utf-8") as f:
            json.dump(
                {
                    "api_url": api_url,
                    "models": models,
                    "results": results,
                },
                f,
                ensure_ascii=False,
                indent=2,
            )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
