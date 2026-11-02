# M0 malformed advertisement evidence

Purpose: test the pinned gix HTTP handshake against malformed and truncated in-memory protocol advertisements, independent of M1 human gates and watched PR22 CI. No concrete network backend, credential broker, object proof or production transport is added.

Only synthetic fixture bodies and tests may change. A successful HTTP response is insufficient evidence of a usable advertisement. Record exact rejection/acceptance behavior; do not silently tighten a third-party protocol parser or claim all Git response handling validated. No user repositories, files, credentials or network requests. Version remains 0.0.17.

Branch starts from local protocol checkpoint ce992db atop PR22 c9afef1; separate reviewable changes will target main after that baseline integrates. Preserve the local moderator protocol commit.
