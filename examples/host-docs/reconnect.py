"""Retry one request after losing its acknowledgement; replay the durable journal."""
import sys
from fixture import HostFixture, LIMITS, POLICY, message, request, send_frame, read_frame, tool


def main(binary, *, export=None):
    outputs = [tool("exec_command", {"command": "printf once >> effects.txt"}), message()]
    with HostFixture(binary, outputs) as host:
        session = host.session()
        submitted = request("submit", session=session,
                            content=[{"type": "input_text", "text": "Record one effect"}],
                            intent={"kind": "new_task", "limits": LIMITS, "policy": POLICY})
        with host.connect() as disconnected:
            send_frame(disconnected, submitted)  # Deliberately discard the acknowledgement.
        host.call(submitted)
        receipt = host.settled(session, submitted)
        assert receipt["status"]["outcome"] == "finished_unverified", receipt
        assert host.call(submitted) == receipt
        assert (host.workspace / "effects.txt").read_text() == "once"
        records = host.journal()
        commands = [record["event"].get("data", {}).get("command", {})
                    for record in records if record["aggregate"] == session]
        admissions = [command["data"] for command in commands if command.get("type") == "submitted"]
        links = [command["data"] for command in commands if command.get("type") == "task_linked"]
        assert len(admissions) == 1 and admissions[0]["id"] == submitted["id"]
        assert links == [{"request": submitted["id"], "task": receipt["status"]["task"]}]
        cursor = records[len(records) // 2]["sequence"]
        expected = [record for record in records if record["sequence"] > cursor]
        with host.connect() as reconnected:
            send_frame(reconnected, request("watch", after=cursor, session=session))
            assert read_frame(reconnected) == {
                "type": "ready",
                "data": {"after": cursor, "through": records[-1]["sequence"]},
            }
            replay = []
            while len(replay) < len(expected):
                frame = read_frame(reconnected)
                if frame["type"] == "journal":
                    replay.append(frame["data"])
        assert replay == expected
        host.assert_provider_consumed()
        if export is not None:
            export(host)
    print("PASS reconnect: one submission/effect; same receipt; exact journal replay")


if __name__ == "__main__":
    main(sys.argv[1])
