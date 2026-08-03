# sail-multiproject

Three minimal projects for workspace-orchestration tests (M6): `a` carries a
healthcheck (so dependents genuinely wait for health), `b` and `c` are plain.
Tests copy these into uniquely-named temp dirs (mast-it-*) — never `up` in
place.
