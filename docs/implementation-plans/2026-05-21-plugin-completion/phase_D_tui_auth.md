# Phase D — TUI Protocol Auth (transparent for local, atproto for remote)

**Plan:** docs/implementation-plans/2026-05-21-plugin-completion/phase_D_tui_auth.md
**Status:** drafted 2026-05-21, revised same day after orual correction on UX goal.
**Depends on:** nothing structural. Phase E provides the remote-TUI auth piece.
**Unblocks:** plugins safely opting into the TUI protocol (discord plugin already does this); future remote TUI / dashboard / mobile clients.

## Motivation + UX goal (orual)

Auth on the TUI ALPN exists because **plugins can opt into the TUI protocol** — the discord plugin already dials `pattern-tui/1` to submit user-messages. That dial needs to be authenticated (we already do this via the plugin-pubkey path) and the same mechanism needs to gate other clients.

**But:** running `pattern` for the first time should require zero extra steps. No "enroll your key," no token paste, no setup ceremony. Local TUI works out of the box.

Three trust tiers, separate code paths:

1. **Local TUI** (loopback, same-user, same-mount): auto-trusted. Transparent. User never sees auth.
2. **Plugin dialing TUI protocol** (e.g. discord plugin): already authed via the existing plugin-pubkey allowlist.
3. **Remote TUI / non-local client**: atproto-based auth (rolls into phase E).

## Current state

- TUI protocol acceptor doesn't check pubkey at all. Anyone reaching the ALPN can dial.
- Plugin-pubkey allowlist exists for `pattern-plugin-guest/1` ALPN but not extended to `pattern-tui/1`.
- Discord plugin dials TUI ALPN with its own pubkey; the acceptor doesn't verify it. Works by accident — no plugin-vs-attacker discrimination today.

## Target state

- Local TUI: spawn daemon, spawn TUI, they talk. No user-visible auth step ever.
- Plugin dialing TUI ALPN: same allowlist that gates plugin-guest ALPN now also gates plugin dials to TUI.
- Remote TUI: deferred to phase E (atproto identity-resolution + shared-secret HMAC, same as remote plugins).

## Design: how local TUI stays invisible

**Mechanism: shared on-disk auth blob.** Daemon and TUI both have access to the mount directory. Daemon writes a fresh keypair to `<mount>/state/local-tui-keypair.json` at startup (0600 perms). TUI reads it on startup. Daemon's allowlist auto-includes whatever pubkey is in that file.

- User never types anything. Both processes use the same mount path; the file is the implicit trust handoff.
- File perms (0600 + same-user) gate access. If you can read the file, you can be the TUI. That's the existing trust assumption already — same as plugin state.json.
- Rotated on daemon restart (regenerated keypair) so a stale leak doesn't persist.

**Alt considered + rejected:** "daemon spawns TUI as child, inherits trust via process tree." Doesn't work because TUI can be launched independently. The on-disk blob is the right shape.

**Alt considered + rejected:** "loopback dialer is auto-trusted." Looks tempting but a malicious local process on the same machine could spoof loopback. The on-disk-keypair-readable-by-this-user is the right granularity.

## Tasks

### D.1 — Daemon writes local-TUI keypair at startup

- On `TidepoolSession::open` (or daemon main): generate a fresh ed25519 keypair
- Write `<mount>/state/local-tui-keypair.json` with `{ pubkey: hex, secret: hex }`, perms 0600
- Auto-add the pubkey to the per-session client allowlist as `ClientRole::LocalTui`
- File is regenerated each daemon start (no persistence across restarts — fresh trust each launch)

### D.2 — TUI reads keypair on startup

- On TUI launch, locate the mount dir (existing logic via `--mount` flag / cwd discovery)
- Read `state/local-tui-keypair.json`; use it as the iroh endpoint identity
- If file missing: print clear error suggesting daemon isn't running, exit

### D.3 — `pattern-tui/1` acceptor checks the client allowlist

- Acceptor consults `ClientRouteTable` (or extend `PluginRouteTable` if shapes converge)
- For dials matching the LocalTui pubkey: accept
- For dials matching a plugin pubkey in `PluginRouteTable` with `dial-channels` declaration including `tui`: accept (this is the discord-plugin path — already declared in its manifest)
- For dials matching neither: `AcceptError::NotAllowed` with the dialing pubkey logged for debugging

### D.4 — Plugin dial-channels manifest field plumbing

- Confirm the existing `dial-channels` manifest field (e.g. `dial-channels "tui"` in plugin KDL) is parsed + populated into the plugin's allowlist entry as a list of additional ALPNs this plugin is allowed to dial
- Acceptor for `pattern-tui/1` consults this when checking plugin pubkeys

### D.5 — Remote-TUI auth (defer to phase E)

Phase E's atproto-record-discovery + shared-secret-HMAC handshake is the substrate for remote TUI. When a non-local client (no entry in LocalTui or PluginRouteTable) dials `pattern-tui/1`, the acceptor falls through to phase E's auth path. Until phase E lands, non-local TUI is rejected with a clear "remote TUI requires atproto-discovery setup" error.

## Acceptance criteria

- Fresh `pattern` install + `pattern tui` → works without any user-visible auth step
- Daemon restart → TUI keypair regenerated, TUI on next launch picks it up, still works
- Discord plugin (with `dial-channels "tui"` in manifest) can submit user-messages via TUI protocol
- A different process attempting to dial `pattern-tui/1` with neither the local-TUI keypair nor a recognized plugin pubkey is rejected
- No `pattern client enroll` CLI; no token paste; no user-visible auth ceremony for the local case

## Gotchas

- **Don't add user-visible auth ceremony.** That was the wrong instinct in the first draft. The threat model for local-same-user is "if you can read 0600 files in the mount, you're already trusted." Match that.
- **Mount directory must be discoverable by TUI.** Either via explicit `--mount` flag, env var, or cwd walk. Existing TUI launcher already does this; don't break the discovery logic.
- **Permissions file mode**: 0600. Both keypair AND state.json should be 0600. If the daemon writes 0644, audit log it as a security regression.
- **Don't persist the local-TUI keypair across daemon restarts.** Regenerate each time. A long-lived disk artifact is a stale credential waiting to be leaked. (Plugin keypairs are different — they're installed once and persist; local-TUI is ephemeral by design.)
- **Don't share the same keypair across mounts.** Each mount has its own state dir → its own local-TUI keypair → its own allowlist.
- **Plugin dialing TUI ALPN** uses the plugin's installed keypair, not a TUI-specific one. The `dial-channels` manifest field is the opt-in.
- **Existing daemon code** that constructs the plugin acceptor or TUI acceptor needs to share the allowlist-check helper — don't duplicate the pubkey-verification logic.

## Out of scope

- Remote TUI (phase E)
- Multi-user mounts (one mount = one user trust matrix for v1)
- TUI keypair rotation during a daemon's lifetime (would only matter for long-lived daemons; cost is daemon restart, which is cheap)
- Browser-based TUI (different identity model — phase E or later)

## Verification before declaring done

1. Fresh mount + `pattern daemon` + `pattern tui`: works, no prompts
2. Restart `pattern daemon`, restart `pattern tui`: still works, no prompts (each restart = new keypair, both pick it up via the state file)
3. Discord plugin (configured + installed) can post to TUI protocol — already worked, must still work
4. Hand-rolled iroh client dialing `pattern-tui/1` with a random keypair: rejected with NotAllowed + dialing pubkey logged
5. Inspect file perms on `state/local-tui-keypair.json`: 0600
