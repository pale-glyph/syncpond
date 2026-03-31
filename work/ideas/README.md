# Ideas Index

This directory contains potential improvement ideas for the syncpond project, organized by category.

| File | Scope | Items |
|---|---|---|
| [bugs.md](bugs.md) | Concrete bugs to fix | 5 |
| [security.md](security.md) | Security hardening | 7 |
| [performance.md](performance.md) | Performance & scalability | 5 |
| [features.md](features.md) | Missing / incomplete features | 9 |
| [devops.md](devops.md) | Operational & deployment | 8 |
| [client.md](client.md) | TypeScript client | 8 |
| [architecture.md](architecture.md) | Architecture & design | 8 |

---

## High-Priority Quick Wins

Items that are low-effort and high-impact:

1. **Fix `constant_time_eq` bug** (`bugs.md` §1) — security-sensitive and a one-line fix.
2. **Fix script directory names** (`bugs.md` §2, §3) — scripts are currently non-functional.
3. **Fix `auth_failed` → `auth_error` in docs** (`bugs.md` §4) — client implementers will hit this.
4. **Fix example config JWT key** (`bugs.md` §5) — confusing runtime error for new developers.
5. **Replace `constant_time_eq` with `subtle` crate** (`security.md` §1) — eliminates the bug and the risk permanently.
6. **Add `RUST_LOG` env-filter to tracing init** (`devops.md` §7) — near-zero effort, high operational value.
7. **Bind command API to loopback in example config** (`security.md` §2) — one-line config change.
8. **Implement catch-up sync via `room_delta`** (`features.md` §1, `performance.md` §5) — already fully implemented in `state.rs`, just needs to be wired into `ws.rs`.
