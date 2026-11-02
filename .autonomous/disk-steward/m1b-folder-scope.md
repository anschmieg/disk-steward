# M1B selected-folder metadata boundary

Purpose: let a user add a folder to a review plan without implying its children were inventoried. This parallels the blocked single-file observation, with the same preservation-first default and no cleanup authority. It addresses the add-folder editor surface while live recursive discovery remains M2.

Use a separate typed native command with profile/revision only and a native single-folder picker. Observe only leaf metadata, never enumerate children, hash/read contents or hydrate files. Record a parentless folder with fresh opaque IDs, bounded display name, unknown logical/recovery size, default Keep, native-metadata-only and contents-not-inventoried blockers. No source path, scan grant or operation binding is retained. Repeated selections remain separate blocked observations.

General editor append, revision, undo/redo and immutable reviews must preserve existing choices and survivor/exclusion state. A zero-child folder placeholder must not be presented as empty or ready for removal. Native cancellation and stale requests must not change the plan. Global remembered exclusions remain a distinct setting with separate confirmations.

Work starts on feat/m1b-folder-metadata based on5c50e8c/PR5, matching0.0.7 versions. Use disposable fixtures only. M1B human pilots and full discovery eligibility remain open; no release.
