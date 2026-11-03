# M0 blocked pack-sink deadline proof

Test-only child runs actual candidate discovery/lsrefs/fetch over privateTLS, accepts exactlyPACKprefix4bytes in a closedmemorysink, then signals readiness only when candidate attempts the nextsinkwrite and intentionallyblocks indefinitely. Parent launches only exactignoredchildtest and boundedstdout, proves childremainsblockedthrough3secouterdeadline, kills/reaps and joinsreader beforeassertions. Reuse existing boundedcandidateGETdeadlineparenthelper; no source/externalprocess/userfile/networkendpoint/renderer paths. No filesystemcleanup and no subprocessdescendantprocesses in child (onlythreads).

This proves parentprocessstoparoundblockedtestwrites only, not hardOSkilllatency, durableworker/storeintegration, fileflush/powerloss, realGitHub/privateauth, eligibility or milestonepass. Cooperativecancellation cannotinterruptarbitraryWrite; productionouterworker remainsrequired.

Luna owns pack_tests.rs child+parenttest only; root refactors existing parenthelper/lib/docs/scope/progress and reviews codebeforeexecution. Branch atop558884fb7/mainb51a2df baseline, localnotes preserved; existing45–55watchesnotpolled/nooverlappingbuilds/CARGO_INCREMENTAL=0/2GiBreserve. Host8.2GiBfree/version0.0.17/no releases.
