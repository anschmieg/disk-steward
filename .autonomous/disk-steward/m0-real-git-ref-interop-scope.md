# M0 real Git ref interoperability

Test the actual candidate observe_refs against system Git upload-pack through private loopback TLS. Create only a private exclusive task-owned temporary root/bare SHA1 repo with empty templates, an empty-tree commit and annotated tag. Fixed /usr/bin/git commands only, cleared child environment/global/system config isolated, no hooks/helpers/signing/remotes/source clones. Feed only fixed bounded stdin and validated fixture object IDs. Capture stdout bounded, supervise fixed Git children with deadlines/kill/wait/reap and join readers before failure assertions. Review concrete subprocess/filesystem code before running.

TLS fixture must observe correct GET/POST target and Git-Protocol v2 header, route bodies to fixed owned-repo upload-pack --stateless-rpc with GIT_PROTOCOL version=2, and return real output. Assert HEAD/main/tag IDs, symref/peeled metadata and both HTTP responses finalized. Task-owned root identity/mode checked before cleanup; refuse replacement. No arbitrary endpoint/path/CLI/usercredential inputs.

Proves only local ref protocol interoperability on observed Git/platform, not real GitHub/private auth, fetching/object completeness, broad Git version matrix, worker/process-tree or milestone acceptance. Version0.0.17/no releases.

Branch test/m0-real-git-ref-interop atop PR50 7546cc6; main finalbaseline1c05a07 through pendingstack. Bounded Luna owns newtests/git_interop.rs only; root libmodule/docs/scope/progress/source review beforeexecution. No newdependencies/duplicateagents/overlappingbuilds. Host11GiBfree, Git2.54.0 AppleGit157 observed; watched42–50 notpolled.
