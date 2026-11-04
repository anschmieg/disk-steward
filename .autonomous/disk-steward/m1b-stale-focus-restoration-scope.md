# M1B: invalidate stale queued editor focus restoration

Build on PR104, main eventual baseline. AdvancedPlanPanel revision focus restoration queues an animation frame without cleanup, clears its pending flag before it runs, and captures old rows/context. A newer busy revision can leave the old callback focusing a disabled-control fallback row instead of the user's newer Search focus. Unmount can leave its global DOM query matching another editor.

Cancel and invalidate pending frames on effect replacement/unmount, and clear pending intent only in a current callback. Preserve pending restoration across a busy interval; capture latest rows/context on reschedule. If focus context changes after scheduling, respect the newer explicit focus rather than restoring the older target. No edit, choice, approval, execution or backend changes.

Use controlled RAF queues (including callbacks invoked despite cancellation) to reproduce stale-frame focus theft, busy rescheduling, newer focus changes and unmount isolation. Preserve existing membership-removal/large-plan focus tests. Validate the new regression fails against old component and the final frontend test/type/build/format checks pass. Component evidence is not native WebView/accessibility/performance or a three-human review pilot; M1B remains open. Version0.0.x/no release/no user-file operations.

Follow-up: scope focus-key queries to the current panel's root section. A current frame in a second mounted panel must not focus a first panel with matching item IDs. The controlled two-panel regression fails against ddb9479 and passes with the scoped query. This extends the same restoration boundary without changing user choices or execution authority.
