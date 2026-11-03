# Development rules

- Follow ROADMAP.md and the specifications. Reconsider the application purpose, milestone contribution, scope cornerstones, and failure cases before each milestone; record evidence and gaps before marking it complete.
- Versions stay at `0.0.x` until Adrian explicitly authorizes a minor release. Do not publish releases automatically.
- Use feature branches and reviewable pull requests. Keep main as the integration baseline.
- Delegate bounded implementation/investigation tasks primarily to GPT-6 Luna, using the lowest reasoning effort adequate to the task. The coordinating agent reviews integration and verification.
- The product investigates storage and builds editable, evidence-backed cleanup plans. No unattended destructive actions or execution based solely on model confidence.
- No generic renderer filesystem, shell, SQL, credential, or execution access. Do not implement real cleanup outside a validated milestone's scope.
- Run appropriate formatting, type, build, and behavioral checks. Record failures and unverified platform claims honestly.

## Durable progress merges

`.autonomous/disk-steward/progress.md` is an append-only evidence log. Its exact
path uses Git's built-in union merge driver to retain concurrent additions.
After merging, verify that all dated entries from both parent versions remain
and append an explicit current branch/head checkpoint. Union output can reorder
or duplicate entries: line order alone never establishes the active checkpoint.
Review edits or removals of existing evidence manually; this driver does not
resolve contradictory claims. Other files retain normal merge behavior.
