# Knowledge Modules

This directory contains implementation-focused documentation for each migrated C module.

## Quick Reference

| Module | Purpose | Status |
|--------|---------|--------|
| [fdwatch](fdwatch.md) | I/O multiplexing abstraction | migrated |
| [libhttpd](libhttpd.md) | HTTP protocol engine | migrated |
| [match](match.md) | Shell-style glob matching | migrated |
| [mime](mime.md) | MIME type/encoding lookup | migrated |
| [mmc](mmc.md) | Memory-mapped file cache | migrated |
| [tdate_parse](tdate_parse.md) | HTTP date parsing | migrated |
| [thttpd](thttpd.md) | Server core/event loop | migrated |
| [thttpd_migrate](thttpd_migrate.md) | Migration proxy | implemented |
| [timers](timers.md) | Timer wheel | migrated |

## About This Directory

Each module file contains:
- Source mapping (legacy C → Rust)
- Implementation status
- What the module does
- Key implementation decisions
- Size metrics showing compression/modernization benefits

The `knowledge/` directory complements the main `docs/` directory by providing module-level technical detail vs. the architectural/system-level documentation in `docs/`.

## See also

- [docs/MIGRATION.md](../../docs/MIGRATION.md) — migration architecture
- [docs/RISKS.md](../../docs/RISKS.md) — current gaps
- [README.md](../../README.md) — project overview
