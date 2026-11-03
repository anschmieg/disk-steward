# M0 smart HTTP failure semantics

Tighten the inactive candidate to HTTP200 successful smart GET/POST responses only. Git HTTP documentation requires200/304 discovery semantics; this candidate has no cache/conditional revalidation and supports200 only, rejecting all other2xx/304 without body success. HTTP401/403 are access failures with PermissionDenied; retain that kind through the Git/ref observation facade. Preserve timeout, limit and cancellation I/O kinds while sanitizing all messages. No server body/header/ref/credential context in public errors.

Primary protocol source: https://git-scm.com/docs/http-protocol (smart-server response status and access denial). No credential retry, ambient helper, redirect, cache, newendpoint or app/renderer integration. Private loopbackTLS and in-memory error fixtures only. This does not pass actual privateGitHub/revocation/fullGit/M0 gates.

Branch fix/m0-smart-http-failures atop PR49 4314f94; main finalbaseline15f0de1 through stack. Luna owns productionlib/refs/errorunit sources/README; root owns nestedtests/failure_tests.rs, scope/progress/review before execution. No duplicateagents/builds; host10GiBfree, no usercachecleanup. Focused checks/current-headCI; watched41–49 notpolled.
