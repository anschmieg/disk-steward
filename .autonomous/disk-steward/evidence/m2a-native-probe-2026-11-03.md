# Native explorer probe — 2026-11-03

Result: **debug bundle built; desktop interaction unverified**. This does not pass M2A, native accessibility, usability, cancellation latency or MVP acceptance.

The exact source head was `20e88a382c36e2b78f7f4d5934ebd86c43bcfd48` (PR120). A task-local Tauri config override produced `Disk Steward MVP Probe 20e88a3.app`, version0.0.17, with effective `CFBundleIdentifier=com.disksteward.probe.mvp20e88a3`. The bundle identifier and executable were read from the built Info.plist before launch. Build command: `npm run tauri -- build --debug --bundles app --config <task-owned temporary override>`. Build succeeded and produced a42.13MiB app bundle. No release, signing/notarization claim or installed-app replacement occurred.

Read-only source investigation confirmed example/editor/exclusion/fixture states resolve through Tauri app_data_dir; the unique identifier separates this probe's stores from the ordinary app. In-memory folder/summary state is also instance-owned. This establishes the intended isolation, not a native workflow pass.

A task-owned temporary fixture contained a three-byte file and hard-link alias, a nested five-byte file, a hidden file and a symlink. Intended interactions were native picker selection, navigation, summary, current-page metadata import, retained scope and clear. No picker or interaction was reached.

Desktop automation selected only the exact probe bundle path, then its proven unique identifier. Both returned `Computer Use server error -10005: cgWindowNotFound`. Inventory recognized the unique app as running. The optional documented `cua.computer.launch_app` activation entry point was unavailable (`is not a function`). No generic installed Disk Steward app selection or fallback UI scripting was used.

Process inspection established PID45696's exact executable path inside the task-owned bundle. A one-second process sample identified the same bundle/version and showed its main thread waiting in the AppKit event loop. This distinguishes a running process from a verified visible window; it does not establish whether the cause is app behavior, desktop/session state or automation access. Do not label this an app failure or a passing UI probe without further evidence.

The fixture manifest's exact bytes, hard-link identity and symlink remained unchanged. Only the proven task-owned PID was terminated. Task artifacts remain available locally; no user caches or unrelated app data were removed. Host free space remained about10GiB, above the2GiB reserve. The probe's existing automated validation remains71native+68frontend+11core tests and strict checks from PR120; these do not substitute for the missing native interaction evidence.

Next native gate: open the uniquely named probe bundle from `src-tauri/target/debug/bundle/macos/` in an interactive macOS session and verify whether a window appears, then run the disposable picker/explorer flow. If it appears manually, restore desktop automation access before claiming automated interaction coverage. Native VoiceOver, representative pilot, real cloud/provider behavior, durable scan checkpoint/pause/resume and remaining roadmap gates are still open. Meaningful safe implementation remains available; the user-authorized automation stop condition has not been reached.
