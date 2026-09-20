"""A native write changes live bytes, but is not a verified completion."""
import sys
from fixture import HostFixture, message, tool


def main(binary):
    outputs = [tool("write_file", {"operation": "replace", "path": "result.txt",
               "expected": {"kind": "absent"}, "content": "local result\n"}), message()]
    with HostFixture(binary, outputs) as host:
        session, receipt = host.run()
        assert receipt["status"]["outcome"] == "finished_unverified", receipt
        task = host.query("task", id=receipt["status"]["task"])
        assert task["outcome"] == "finished_unverified"
        assert task["certificate"] is None and task["evidence"] == 0
        assert (host.workspace / "result.txt").read_text() == "local result\n"
        assert host.query("session", id=session)["outcome"] == "finished_unverified"
        host.assert_provider_consumed()
    print("PASS native: live bytes changed; FinishedUnverified; no certificate")


if __name__ == "__main__":
    main(sys.argv[1])
