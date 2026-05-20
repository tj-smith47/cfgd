---
paths: ["**/state/**/*.rs", "**/gateway/**/*.rs", "**/*db*.rs"]
---
# cfgd Database Conventions

All SQLite databases (`StateStore` in `cfgd-core`, `GatewayDb` in `cfgd-operator` gateway) must:

- Set `PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;` on open
- Use versioned migrations (not ad-hoc `CREATE TABLE IF NOT EXISTS`)
- Use `cfgd_core::utc_now_iso8601()` for timestamps — no local wrappers
- Hash with `cfgd_core::sha256_hex()` — not inline `Sha256::new()` + `update()` + `finalize()` chains, and not `Sha256::digest()` directly outside the helper

See `shared-utils.md` for the timestamp and hashing helpers.
