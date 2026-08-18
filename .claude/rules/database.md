---
paths: ["crates/**/state/**/*.rs", "crates/**/gateway/**/*.rs", "crates/**/*db*.rs"]
---
# cfgd Database Conventions

All SQLite databases (`StateStore` in `cfgd-core`, `GatewayDb` in `cfgd-operator` gateway) must:

- Set `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;` on open
- `synchronous=NORMAL` is valid **only because `WAL` is set on the same line**. Under WAL, `NORMAL` still cannot lose or corrupt a committed transaction when the process dies — the WAL is durable in the page cache and replayed on the next open — so the only window it trades away is an OS crash or power loss between commit and checkpoint. Under rollback-journal mode the same pragma can corrupt the database, so never carry it to a connection that is not in WAL (`open_in_memory` sets neither, and needs neither)
- Batch a **loop** of writes through `StateStore::in_transaction`, not one implicit per-statement transaction per row: an apply records one row per action and one backup blob per touched file, and each implicit commit is its own WAL write. `in_transaction` rolls back on `Err` and on panic. Do NOT batch a write whose value is being on disk before the next thing happens — the journal's per-action begin/finish rows and the pre-action file backups are what a crashed apply is reconstructed from
- Use versioned migrations (not ad-hoc `CREATE TABLE IF NOT EXISTS`)
- Use `cfgd_core::utc_now_iso8601()` for timestamps — no local wrappers
- Hash with `cfgd_core::sha256_hex()` — not inline `Sha256::new()` + `update()` + `finalize()` chains, and not `Sha256::digest()` directly outside the helper

See `shared-utils.md` for the timestamp and hashing helpers.
