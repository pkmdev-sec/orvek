import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import run_paired


class Accounting(unittest.TestCase):
    def test_unknown_cost_and_cache_stay_unknown(self):
        self.assertIsNone(run_paired.total([None, 0]))
        self.assertIsNone(run_paired.total([]))
        self.assertEqual(run_paired.total([0, 4]), 4)
        observed = {"request":"request","receipt":{"task":"task"},"usage":{"input_tokens":10,"cached_input_tokens":None,"output_tokens":2,"reasoning_tokens":None},
            "cost":None,"issues":["missing_provider_receipt"],"log_complete":True,"retrievals":1,"retries":None,"child_outcomes":[]}
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory)/"run.jsonl"
            # Real T03 event wrapping; proposal and retrieval both incur model work.
            def event(call,name):
                return {"type":"event","data":{"type":"journal","data":{"aggregate":"session","event":{"data":{"command":{
                    "type":"response","data":{"request":"request","items":[{"type":"function_call","call_id":call,"name":name}]}}}}}}}
            log.write_text("\n".join(json.dumps(event(call,name)) for call,name in [("a","transition_context"),("b","read_context")]))
            with patch.object(run_paired.measurements,"parse_log",return_value=observed):
                measured = run_paired.summarize_attempt(None,log,[{},{}],5)
            self.assertEqual(measured["model_calls"],2)
            self.assertEqual(measured["summary_proposal_calls"],1)
            self.assertEqual(measured["retrieval_calls"],1)
            self.assertIsNone(measured["provider_receipt_usd"])
            self.assertIsNone(measured["uncached_input_tokens"])
            self.assertIsNone(measured["request_bytes"],"absent receipts cannot be manufactured from reencoded JSON")
            self.assertEqual(measured["usage"]["reasoning_tokens"],None)


if __name__ == "__main__":
    unittest.main()
