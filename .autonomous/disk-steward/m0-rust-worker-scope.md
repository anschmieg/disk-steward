# M0 Rust fixed-child supervision proof

Purpose: add executable Rust subprocess-lifecycle evidence missing from the Python-only resource probe. The standard-library-only workspace probe uses only its own current executable and closed synthetic flood/stall modes. Default is dry-run; explicit --run starts two sequential children. No generic command/path/shell inputs, renderer commands, network, credentials or temporary stores.

Bound capture to fixed cap+1 bytes, observe an outer monotonic deadline, stop and reap the child and join the reader before success. Stall emits a readiness byte before sleeping; flood is finite. Every error path must attempt stop/join and must not falsely report joined state. No process-tree, hostile child, OS kill/wait upper-bound, CPU/memory or real Git ingress/storage enforcement claims. No user data is mutated.

Root reviews error/deadlock paths and bounded integration tests, runs probe tests/fmt/strictClippy; full workspace CI remains required. Version stays 0.0.17. Branch starts from PR24head4c29594, preserving stack; main remains final baseline, retarget after lower layers integrate. Watched PRs are not polled by recurring wakes.
