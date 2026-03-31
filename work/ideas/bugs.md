# Bug Fixes

Concrete bugs identified in the current codebase.

---

## ✅ 1. `constant_time_eq` compares wrong iterators

**File:** `server/src/main.rs`

When the two strings have different lengths, the function zips `a` with `a` instead of `a` with `b`. The length-based early return still prevents a timing oracle, but the byte comparison loop is logically wrong and would pass any same-length string as equal to a different-length one if re-used in a context without the length guard.

```rust
// Current (wrong)
a.iter().zip(a.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0

// Should be
a.iter().zip(b.iter()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
```

The simplest fix is to use the `subtle` crate's `ConstantTimeEq` implementation rather than rolling a custom one.

---

## ✅ 2. Build script uses wrong directory name

**File:** `scripts/build_push.sh`

The script `cd`s into `$REPO_ROOT/syncpond-server` which does not exist. The actual directory is `$REPO_ROOT/server`.

```bash
# Current (broken)
cd "$REPO_ROOT/syncpond-server"

# Should be
cd "$REPO_ROOT/server"
```

---

## ✅ 3. Publish script uses wrong directory name

**File:** `scripts/publish_ts_client.sh`

`CLIENT_DIR` is set to `$REPO_ROOT/syncpond-client`. The actual directory is `$REPO_ROOT/client`.

```bash
# Current (broken)
CLIENT_DIR="$REPO_ROOT/syncpond-client"

# Should be
CLIENT_DIR="$REPO_ROOT/client"
```

---

## ✅ 4. Doc/code mismatch: `auth_failed` vs `auth_error`

**File:** `server/doc/client-protocol.md`

The documentation refers to an `auth_failed` message type, but the server (`ws.rs`) only ever sends `{ type: "auth_error", ... }`. Clients implementing to the documented type name will silently miss auth failures.

Fix: update `client-protocol.md` to use `auth_error` consistently, and search for any other stale references.

---

## ✅ 5. Example config JWT key is too short

**File:** `server/example/config.yaml`

```yaml
jwt_key: FAKEKEY   # 7 bytes — JwtKeyTooShort at runtime
```

`TOKEN.GEN` requires a key of ≥ 32 bytes. A developer copying this config and calling `TOKEN.GEN` will receive a runtime error with no obvious explanation. The example should either use a valid placeholder or include an inline comment warning.
