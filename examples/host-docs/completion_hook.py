"""The configured local hook has durable receipts, separate from task truth."""
import json
import sys
from fixture import HostFixture, message, wait_until


def main(binary):
    # No external notification. Append one payload, then deliberately fail the shell.
    with HostFixture(binary, [message()], hook="cat >> completion.jsonl; exit 9") as host:
        session, receipt = host.run()
        assert receipt["status"]["outcome"] == "finished_unverified", receipt

        def delivered():
            events = [record["event"].get("data", {}).get("command", {})
                      for record in host.journal() if record["aggregate"] == session]
            hooks = [event["data"] for event in events if event.get("type") == "completion_hook"]
            return hooks if any(event["type"] == "finished" for event in hooks) else None

        hooks = wait_until(delivered)
        assert [event["type"] for event in hooks] == ["armed", "claimed", "finished"], hooks
        payload, = [json.loads(line) for line in (host.workspace / "completion.jsonl").read_text().splitlines()]
        assert payload == hooks[1]["data"]["payload"]
        assert payload["session"] == session and payload["request"] == receipt["id"]
        assert payload["task"] == receipt["status"]["task"]
        assert payload["outcome"] == "finished_unverified" and payload["version"] == 1
        assert hooks[2]["data"]["result"] == {"type": "failed", "data": {"exit_code": 9}}
        task = host.query("task", id=payload["task"])
        assert task["outcome"] == "finished_unverified" and task["certificate"] is None
        host.assert_provider_consumed()
    print("PASS completion hook: one local payload; failed receipt; unchanged task outcome")


if __name__ == "__main__":
    main(sys.argv[1])
