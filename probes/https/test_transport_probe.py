import unittest
import time

try:
    from https.transport_probe import ProbeError, probe, validate_endpoint
except ModuleNotFoundError:
    from transport_probe import ProbeError, probe, validate_endpoint

URL = "https://github.com/example/project.git/info/refs?service=git-upload-pack"


class FakeResponse:
    def __init__(self, status=200, body=b"abc", headers=None):
        self.status = status
        self.body = body
        self.headers = headers or {}
        self.closed = False

    def read(self, size):
        chunk, self.body = self.body[:size], self.body[size:]
        return chunk

    def close(self):
        self.closed = True


class DelayedResponse(FakeResponse):
    def __init__(self, *, delay, body=b""):
        super().__init__(body=body)
        self.delay = delay

    def read(self, size):
        time.sleep(self.delay)
        return super().read(size)


class HttpsProbeTests(unittest.TestCase):
    def test_network_disabled_by_default(self):
        with self.assertRaisesRegex(ProbeError, "network disabled"):
            probe(URL)

    def test_success_and_auth_is_header_only(self):
        response = FakeResponse(headers={"Content-Length": "3", "Content-Type": "application/x-git-upload-pack-advertisement"})
        observed = {}

        def fake(url, headers, timeout):
            observed.update(headers)
            return response

        result = probe(URL, transport=fake, token="secret-token")
        self.assertEqual(result.bytes_read, 3)
        self.assertNotIn("secret-token", repr(result))
        self.assertIn("Authorization", observed)
        self.assertNotIn("secret-token", observed["Authorization"])
        self.assertTrue(response.closed)

    def test_declared_oversize_rejected_before_body_read(self):
        response = FakeResponse(body=b"never", headers={"Content-Length": "11"})
        with self.assertRaisesRegex(ProbeError, "byte limit"):
            probe(URL, transport=lambda *_: response, max_bytes=10)
        self.assertEqual(response.body, b"never")

    def test_streaming_oversize_rejected(self):
        response = FakeResponse(body=b"0123456789X")
        with self.assertRaisesRegex(ProbeError, "byte limit"):
            probe(URL, transport=lambda *_: response, max_bytes=10)

    def test_truncated_body_is_explicit(self):
        response = FakeResponse(body=b"ab", headers={"Content-Length": "3"})
        with self.assertRaisesRegex(ProbeError, "truncated"):
            probe(URL, transport=lambda *_: response)

    def test_redirect_rejected_and_closed(self):
        response = FakeResponse(status=302, headers={"Location": "https://evil.example/"})
        with self.assertRaisesRegex(ProbeError, "redirect"):
            probe(URL, transport=lambda *_: response)
        self.assertTrue(response.closed)

    def test_authenticated_redirect_rejected_without_following_or_disclosure(self):
        secret = "do-not-leak-token"
        response = FakeResponse(status=302, headers={"Location": f"https://evil.example/{secret}"})
        calls = []

        def fake(url, headers, timeout):
            calls.append((url, dict(headers), timeout))
            return response

        with self.assertRaisesRegex(ProbeError, "redirect rejected") as caught:
            probe(URL, transport=fake, token=secret)
        self.assertEqual(len(calls), 1)
        self.assertIn("Authorization", calls[0][1])
        self.assertNotIn(secret, str(caught.exception))
        self.assertTrue(response.closed)

    def test_malformed_or_unsupported_urls_rejected(self):
        bad = [
            "http://github.com/example/project.git/info/refs?service=git-upload-pack",
            "https://evil.example/example/project.git/info/refs?service=git-upload-pack",
            "https://user:pass@github.com/example/project.git/info/refs?service=git-upload-pack",
            "https://github.com/example/project.git/info/refs",
            "https://github.com/a/b/extra/info/refs?service=git-upload-pack",
            "https://github.com/../project.git/info/refs?service=git-upload-pack",
            "https://github.com/example/..git/info/refs?service=git-upload-pack",
            "not a url",
        ]
        for url in bad:
            with self.subTest(url=url), self.assertRaises(ProbeError):
                validate_endpoint(url)

    def test_deadline_covers_delayed_body_and_eof(self):
        for body in (b"late bytes", b""):
            response = DelayedResponse(delay=0.02, body=body)
            with self.subTest(body=body), self.assertRaisesRegex(ProbeError, "deadline"):
                probe(URL, transport=lambda *_: response, deadline_seconds=0.005)
            self.assertTrue(response.closed)

    def test_invalid_limits_rejected(self):
        for deadline in (float("nan"), float("inf"), float("-inf"), 0, -1, True):
            with self.subTest(deadline=deadline), self.assertRaisesRegex(ProbeError, "invalid probe limits"):
                probe(URL, transport=lambda *_: FakeResponse(), deadline_seconds=deadline)
        for limit in (0, -1, True, 1.5):
            with self.subTest(limit=limit), self.assertRaisesRegex(ProbeError, "invalid probe limits"):
                probe(URL, transport=lambda *_: FakeResponse(), max_bytes=limit)

    def test_transport_error_is_sanitized(self):
        secret = "top-secret-token"

        def fail(*_):
            raise RuntimeError(f"request failed with Authorization: {secret}")

        with self.assertRaises(ProbeError) as caught:
            probe(URL, transport=fail, token=secret)
        self.assertNotIn(secret, str(caught.exception))
        self.assertEqual(str(caught.exception), "transport failed")


if __name__ == "__main__":
    unittest.main()
