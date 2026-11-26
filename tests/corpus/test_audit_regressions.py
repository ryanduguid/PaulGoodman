"""Fabricated boundary cases from the September portfolio audit."""
import base64
import hashlib
import io
import json
import unittest
from pathlib import Path
from unittest import mock

from fadden import http_fetch


class AuditRegressionTests(unittest.TestCase):
    def test_non_object_json_retries_and_fails_closed(self):
        for body in (b"null", b"[]", b"1", b'"text"', b"true"):
            with self.subTest(body=body), mock.patch.object(
                http_fetch.urllib.request, "urlopen", side_effect=lambda *a, **k: io.BytesIO(body)
            ) as request, mock.patch.object(http_fetch.time, "sleep"):
                self.assertIsNone(http_fetch.fetch_json("https://example.invalid"))
                self.assertEqual(request.call_count, http_fetch.TRIES)

    def test_fixture_retained_response_matches_its_digest_and_length(self):
        fixture = Path(__file__).parent / "fixtures/live-evidence/evidence-bundle.v2.json"
        bundle = json.loads(fixture.read_text(encoding="utf-8"))
        payload = base64.b64decode(bundle["primary_response_base64"], validate=True)
        request = bundle["capture_result"]["requests"][0]
        digest = "sha256:" + hashlib.sha256(payload).hexdigest()
        self.assertEqual(request["response_length"], len(payload))
        self.assertEqual(request["response_sha256"], digest)
        self.assertEqual(bundle["observation"]["primary_response_sha256"], digest)
