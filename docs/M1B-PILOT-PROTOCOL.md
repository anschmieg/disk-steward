# M1B synthetic review pilot protocol

## Purpose and pass criteria

Run this same moderated session with three representative people, separately. The target is for each person to finish every critical task within 15 minutes, make every critical preservation decision correctly, and need no moderator intervention to avoid unintended removal. Do not average times or correctness across people. A missed critical decision, a moderator rescue, or exceeding 15 minutes is a failed participant result to investigate; recording data alone never passes the pilot. Record any changed target or protocol before using it.

This is a usability review of a synthetic plan, not a cleanup trial. The profile contains 1,000 findings in 25 groups. It includes project notes suggested Keep; generated cache; managed data with a blocker; an unclassified installer; duplicate pairs; shared-storage aliases; and further synthetic findings. No user inventory is read. Do not stage, restore, purge, or otherwise operate on user files.

## Before each participant

1. Arrange a fresh, disposable, isolated app-data environment for this participant, and verify it is separate from every other participant's environment. Prepare it outside this protocol using the project's supported setup process; this document defines no reset command. Do not start until the synthetic pilot profile is available at its expected initial state.
2. The fixture does not seed user overrides. Prepare a baseline user Keep decision on `valuable-project-notes.md` in Example project 01 before timing, then verify it in the loaded editor; record the initial revision and selected count. Do not assume a fresh app-data environment creates this override.
3. Verify the running app's exact identity and its isolated data location before the session. The previous validation proved the unique bundle, process and isolated control-store paths, but could not attach to its window; native recorder interaction remains unverified. A build or display name alone does not prove the intended instance.
4. Have the observation sheet below ready. Assign an anonymous code such as P1, P2, P3; do not put names or identifying details in the recorder. Use the native pilot profile, showing “1,000 findings · 25 groups.” The 10,000-item stress profile is not the participant dataset.
5. Tell the participant: “This is a synthetic example. Please think aloud and make the choices you would trust. I can clarify what a control does, but I will not point you to an answer.” Start the wall-clock session immediately before the first task. The target is 15 minutes for the complete critical task set.

## Moderator tasks and prompts

Give one task at a time, without naming the control or target row. Note start/end time for each, first action, errors, confusion, assistance, and any reversal. Do not fix their choices for them.

1. **Preserve the project.** “Find the project notes and make sure they will be kept.” Pass only if the participant identifies `valuable-project-notes.md` under Example project 01 and leaves it Keep/not selected.
2. **Exclude one descendant.** “In Example cache 02, keep the valuable project notes out of the cleanup selection while including the eligible generated material.” Observe whether they understand that the descendant exclusion applies to the chosen child entry/entries, not to the folder itself. Pass only if valuable-project-notes.md is explicitly excluded, remains unselected, and generated-cache-entry.bin is selected; the folder's decision must not defeat that exclusion. Ask them to explain the result in their own words.
3. **Add an installer observation.** “Use the add-file action to inspect a disposable installer example.” Provide a moderator-prepared disposable installer file and ask them to select it through the native picker. Observe that it appears as metadata with Keep/default blocked cleanup. If the picker is canceled, note the cancellation and repeat once with the prepared file. Do not use an actual installer or personal file.
4. **Remove and re-add.** Ask them to set generated-cache-entry.bin in Example downloads 03 to Keep, remove that finding from the plan, then restore its membership using Show removed and Add back to plan. Pass only if the same finding returns with its user Keep decision preserved and remains unselected. Ask them to explain whether removal from the plan deletes a file (it does not). Selecting the same file again through the native picker creates a separate metadata observation and is not this task.
5. **Resolve a duplicate pair.** “Keep one copy of an identical pair and include the other.” Pass only if they deliberately set the survivor and understand which member remains protected. If the seeded initial state already has a survivor, ask them to identify it and change it to the other member, then verify the final survivor is correct.
6. **Respect a blocker.** “Try to include the managed library record, then tell me what the interface means.” Pass only if they recognize that the blocker prevents inclusion and do not infer that a selected-looking suggestion overrides it.
7. **Review and explain status.** Ask the participant to freeze/open the review, then explain in their own words what is selected, what is staged, and what space has been recovered. Expected: selected means included in the synthetic proposed plan; this pilot has not staged anything; it has recovered no space. Return to the editor if needed to verify their final choices. Do not imply that estimated bytes or a selected decision are recovered space.

If the participant asks for help, first record their exact question and what they already tried. Give only neutral clarification needed to continue, then mark that task as assisted. Assistance needed to prevent an unintended removal fails the criterion. Moderator explanations can inform diagnosis but cannot convert a failed unaided task into a pass.

## Recorder and observations

Before timing, load/confirm the current editor. Expand **Review pilot recording**, enter the anonymous code, and select **Start recording** just before task 1. The recorder is tab-local; its report is not automatically saved or sent. After task 7, select **Finish recording** and transcribe or retain the displayed report according to the study's local handling practice. It reports elapsed time, initial/final aggregate context, and counts such as clicks, key presses, choices, membership changes, review navigation, metadata additions, cancellations, and failed requests. Counts are input events, not unique decisions or correctness; the report has `humanAssessment: not_recorded` and `executionAvailable: false`.

Complete the manual sheet below for confusion, assistance, reversals, and correctness. Explicitly mark each critical task pass/fail; a recorder report or successful final state cannot reveal a mistake that was later reversed. Do not let the recorder replace wall-clock task timings or observations.

## Blank participant sheet (copy once per person)

Participant code: ________  Date: ________  Moderator: ________

Exact app/bundle identity verified: ________  Isolated app-data location verified: ________

Fresh disposable environment verified: ________  Seeded overrides prepared and verified: ________

Initial profile/items/groups: ________ / ________ / ________  Initial selected count: ________

| Critical task | Start/end or duration | Unaided pass? | Confusion or error, verbatim where useful | Assistance | Reversal / final state |
|---|---|---|---|---|---|
| Preserve project notes as Keep | | | | | |
| Exclude descendant; distinguish folder choice | | | | | |
| Add disposable installer using native picker; blocked/Keep | | | | | |
| Remove then Add back to plan; user Keep retained | | | | | |
| Choose duplicate survivor correctly | | | | | |
| Understand managed-data blocker | | | | | |
| Explain selected vs staged vs recovered | | | | | |

Total elapsed (wall clock): ________  Within 15 min: yes / no

Critical preservation decisions all correct: yes / no  Moderator rescue to prevent unintended removal: yes / no

Recorder report captured locally: yes / no  Recorder elapsed: ________  Clicks: ________  Key presses: ________

Recorder item choices: ________  Group choices: ________  Membership changes: ________  Review navigation: ________

Metadata additions: ________  Canceled selections: ________  Failed requests: ________

Additional observations, reversals, participant explanation, and follow-up: 

____________________________________________________________________________

Participant result: pass / fail / incomplete. Reason and unresolved gaps: 

____________________________________________________________________________

## Known setup and coverage limits

- The synthetic fixture provides a project note, generated-cache entry, and managed-data blocker in every group; an unclassified installer row and shared-storage aliases; and duplicate sets only in the first three groups. It has no seeded user choice overrides; the moderator-prepared baseline supplies one. Duplicate survivor state is mixed: the first set starts without a survivor and later sets have the first member as survivor. Verify the target set before the session. The design requires coverage including 10 blocked groups and five mixed folders; the current fixture exceeds those counts. Record the actual composition rather than claiming exactly 10/5/3.
- The added installer is not the fixture's unclassified installer row. It is a new native-picker metadata observation, default Keep and hard-blocked. Folder additions are also metadata-only and blocked; do not substitute one for the installer task.
- Descendant exclusions are stored on a folder and apply to selected descendant identities. They do not exclude the folder itself. Make this distinction observable in the participant's resulting choices and explanation.
- The current general-review pilot has no staging/recovery execution. The status-comprehension task checks understanding of the product's terms only: selected is a plan decision, staged is a later recoverable operation, and recovered space is measured only after permanent removal and observation. The recorder itself states execution is unavailable. This protocol cannot establish comprehension after a real lifecycle operation.
- Obtain three separate human participants; three runs by one person do not satisfy the gate. The recorder contains no moderator assessment, correctness, confusion, assistance, or reversals, so those fields must be completed manually.
- Prior validation notes say native recorder interaction was not verified because UI attachment failed. Confirm identity and recorder availability before scheduling participants; do not report the recorder as native-validated based on component tests or a build.

## Source references

- [Review usability gate and 15-minute target](../ROADMAP.md#review-usability-is-a-completion-gate)
- [Design review-pilot protocol](../.kiro/specs/macos-cleanup/design.md)
- [Synthetic fixture definition](../crates/plan-model/src/lib.rs)
- [Recorder UI](../src/components/ReviewPilotPanel.tsx) and [aggregate event contract](../src/review-pilot-contract.ts)
- [Current validation status and UI-attachment limitation](M1B-VALIDATION.md)
