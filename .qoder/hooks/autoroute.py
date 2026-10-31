#!/usr/bin/env python3
"""Inject the AUTO_ROUTE orchestration protocol on @AUTO_ROUTE.md prompts."""
import json
import sys

CONTEXT = (
    "AUTO-ROUTE MODE: the user asked to auto-route this task through AUTO_ROUTE.md.\n"
    "1. Read AUTO_ROUTE.md and load the level-router skill before planning.\n"
    "2. Pick the owning rung(s) from AUTO_ROUTE.md sections 3-4 and the authority tier from section 2.\n"
    "3. Dispatch each owning rung with the Agent tool, subagent_type set to the project agent name "
    "(associate-verification, junior-media-writer, senior-finance, senior-design-ux, "
    "chief-design-officer, staff-client-platforms, staff-server, staff-core-systems, "
    "principal-security, fellow-architect). Run independent rungs in parallel in one message; "
    "serialize dependent steps.\n"
    "4. Do not implement a rung's territory in the main thread when that rung exists.\n"
    "5. Run associate-verification before reporting any implementation complete.\n"
    "6. T2 items (destructive ops, deploys, credentials, money movement, publishing) still need "
    "explicit human approval; never self-approve.\n"
    "7. If a rung is not dispatchable as a subagent type, stop and tell the user to run "
    "/agents reload. Do not substitute general-purpose for a rung."
)


def main() -> int:
    raw = sys.stdin.read()
    try:
        event = json.loads(raw)
    except json.JSONDecodeError:
        return 0
    prompt = event.get("prompt") or event.get("user_prompt") or event.get("message") or raw
    if not isinstance(prompt, str):
        prompt = json.dumps(prompt)
    if "AUTO_ROUTE" not in prompt:
        return 0
    json.dump(
        {
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": CONTEXT,
            }
        },
        sys.stdout,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
