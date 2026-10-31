"""Development-only bounded GitHub HTTPS smart-HTTP reachability probe.

Networking is disabled unless ``allow_network=True`` is passed explicitly.
"""
from __future__ import annotations

import re
import math
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Callable, Mapping, Protocol
from urllib.parse import urlsplit

VERSION = "0.0.2"
_DEFAULT_MAX_BYTES = 1024 * 1024
_DEFAULT_DEADLINE_SECONDS = 10.0
_ENDPOINT = re.compile(r"^/([A-Za-z0-9_-][A-Za-z0-9_.-]*)/([A-Za-z0-9_-][A-Za-z0-9_.-]*\.git)/info/refs$")


class ProbeError(RuntimeError):
    """Sanitized probe failure; messages never contain URLs or credentials."""


class Response(Protocol):
    status: int
    headers: Mapping[str, str]

    def read(self, size: int) -> bytes: ...
    def close(self) -> None: ...


Transport = Callable[[str, Mapping[str, str], float], Response]


@dataclass(frozen=True)
class ProbeResult:
    status: int
    bytes_read: int
    content_type: str | None


def validate_endpoint(url: str) -> str:
    """Return a validated smart-HTTP upload-pack advertisement URL."""
    try:
        parsed = urlsplit(url)
        host = parsed.hostname
        # Accessing port can raise ValueError for malformed authorities.
        port = parsed.port
    except (TypeError, ValueError):
        raise ProbeError("unsupported endpoint") from None
    path_match = _ENDPOINT.fullmatch(parsed.path)
    if (parsed.scheme != "https" or host != "github.com" or port not in (None, 443)
            or parsed.username is not None or parsed.password is not None
            or parsed.fragment or parsed.query != "service=git-upload-pack"
            or path_match is None or path_match.group(1) in (".", "..")
            or path_match.group(2) in (".git", "..git")):
        raise ProbeError("unsupported endpoint")
    return url


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _urllib_transport(url: str, headers: Mapping[str, str], timeout: float) -> Response:
    request = urllib.request.Request(url, headers=dict(headers), method="GET")
    opener = urllib.request.build_opener(_NoRedirect)
    try:
        return opener.open(request, timeout=timeout)
    except urllib.error.HTTPError as error:
        # HTTPError is a response object, including redirects. Caller handles status.
        return error


def probe(
    url: str,
    *,
    allow_network: bool = False,
    transport: Transport | None = None,
    token: str | None = None,
    max_bytes: int = _DEFAULT_MAX_BYTES,
    deadline_seconds: float = _DEFAULT_DEADLINE_SECONDS,
) -> ProbeResult:
    """Read a bounded response from a validated GitHub upload-pack endpoint.

    A supplied token is sent only as an Authorization header and is never
    returned or included in errors. This function does not retrieve credentials.
    """
    endpoint = validate_endpoint(url)
    if (not isinstance(max_bytes, int) or isinstance(max_bytes, bool) or max_bytes < 1
            or not isinstance(deadline_seconds, (int, float))
            or isinstance(deadline_seconds, bool) or not math.isfinite(deadline_seconds)
            or deadline_seconds <= 0):
        raise ProbeError("invalid probe limits")
    if transport is None and not allow_network:
        raise ProbeError("network disabled")
    send = transport if transport is not None else _urllib_transport
    headers = {"Accept": "application/x-git-upload-pack-advertisement"}
    if token is not None:
        if not token or "\r" in token or "\n" in token:
            raise ProbeError("invalid authorization value")
        # GitHub accepts x-access-token as the Basic username for token auth.
        import base64
        encoded = base64.b64encode(f"x-access-token:{token}".encode()).decode("ascii")
        headers["Authorization"] = f"Basic {encoded}"

    started = time.monotonic()
    response: Response | None = None
    try:
        response = send(endpoint, headers, deadline_seconds)
        if time.monotonic() - started >= deadline_seconds:
            raise ProbeError("deadline exceeded")
        if response.status in (301, 302, 303, 307, 308):
            raise ProbeError("redirect rejected")
        if response.status < 200 or response.status >= 300:
            raise ProbeError("HTTP request failed")
        declared = response.headers.get("Content-Length") or response.headers.get("content-length")
        if declared is not None:
            try:
                expected = int(declared)
            except (TypeError, ValueError):
                raise ProbeError("invalid response length") from None
            if expected < 0 or expected > max_bytes:
                raise ProbeError("response exceeds byte limit")
        else:
            expected = None
        count = 0
        while True:
            remaining = deadline_seconds - (time.monotonic() - started)
            if remaining <= 0:
                raise ProbeError("deadline exceeded")
            # urllib's socket timeout is per blocking operation. Tighten it to
            # the remaining absolute budget before each streamed read.
            try:
                sock = response.fp.raw._sock
                sock.settimeout(remaining)
            except AttributeError:
                pass
            # Read only up to one byte over budget, to detect streaming overflow.
            read = getattr(response, "read1", response.read)
            chunk = read(min(65536, max_bytes + 1 - count))
            # Include bytes and EOF in the deadline: either can arrive late.
            if time.monotonic() - started >= deadline_seconds:
                raise ProbeError("deadline exceeded")
            if not chunk:
                break
            count += len(chunk)
            if count > max_bytes:
                raise ProbeError("response exceeds byte limit")
        if expected is not None and count != expected:
            raise ProbeError("truncated response")
        content_type = response.headers.get("Content-Type") or response.headers.get("content-type")
        return ProbeResult(response.status, count, content_type)
    except ProbeError:
        raise
    except Exception:
        # Never relay transport exceptions: they may include request headers or URL.
        if time.monotonic() - started >= deadline_seconds:
            raise ProbeError("deadline exceeded") from None
        raise ProbeError("transport failed") from None
    finally:
        if response is not None:
            try:
                response.close()
            except Exception:
                pass
