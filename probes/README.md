# Disposable recovery mechanics probe

Run with `python3 -m unittest discover -s probes -v`.

This development-only probe creates one `TemporaryDirectory` per run and only
its own synthetic fixture files. Its API takes fixture names, never arbitrary
user paths. It simulates app-managed staging by renaming within the temporary
fixture, stores an identity fingerprint in a local fixture manifest, rejects
restore collisions, verifies identity before restore and selective purge, and
reports a simulated interrupted move without deleting or reconciling it
automatically. TemporaryDirectory cleanup removes the entire probe fixture.

This demonstrates fixture mechanics only. It does **not** validate native
macOS Trash, platform file identity, symlink/hard-link behavior, races, durable
crash recovery, controlled-volume space recovery, permissions, or production
safety. It does not use actual Trash or touch user files. No production action
should be enabled based on this probe.
