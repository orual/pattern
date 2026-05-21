# Phase E — Atproto Remote-Plugin Auth

**Plan:** docs/implementation-plans/2026-05-21-plugin-completion/phase_E_atproto_remote_auth.md
**Status:** drafted 2026-05-21 after extended design discussion with orual.
**Depends on:** v3-extensibility Phase 7 (existing plan; provides session record + Constellation backlink discovery).
**Builds on:** Phase A (plugin transport substrate), Phase D (auth path through plugin-pubkey allowlist that this extends to remote-plugin pubkeys).

## Motivation

Remote plugins (running on a different machine, possibly hosted by another party) should be installable + dialable via atproto identity, not local state.json. The design needs to:

1. Discover remote plugin endpoints without configuration files (atproto records as the discovery substrate)
2. Authenticate the relationship cryptographically (not security-by-obscurity)
3. Support both "one user, two of their own things" (self-flow) and "hosted plugin serving many daemons" (hosted-flow)
4. Allow revocation by either side
5. Avoid persistent shared credentials

## Trust model

Three atproto record types under the `systems.atproto.plugin.*` namespace:

| NSID | Role | Lifecycle |
|---|---|---|
| `systems.atproto.plugin.session` | Minimal node presence: node_id + DID | Published always when running; updated on address change |
| `systems.atproto.plugin.service` | Public pairing advertisement + service metadata (commands, capabilities, version, description) | Published by hosted plugins offering public pairing. Optional. Can be deleted to stop accepting new pairings. |
| `systems.atproto.plugin.peer` | Bilateral pairing record: peer_did + my_node_id + proof | Published per pairing, by both sides. Deletion = revocation. |

**Key insight:** pairing establishment requires *both* sides to publish a `peer` record. Dial gating happens by checking record-existence (via Constellation backlink on peer_did) at dial time. Either side deletes their record → relationship broken. Symmetric, no hidden state.

## The pairing ceremony

### Ephemeral token T

Pairing uses a single-use ephemeral token T (e.g. random 256-bit value). T is consumed once during ceremony; doesn't persist beyond the ceremony's lifetime.

**Self-flow (one person pairs their own plugin + daemon):**
- User generates T via `pattern plugin pair` (or equivalent CLI)
- T is shown to user as a copy-pasteable string + peer-DID-and-node-id-and-nonce all encoded together
- User pastes the whole blob into the other side
- Both sides now hold T + expected_peer_did + peer_node_id

**Hosted-flow (plugin operator serves multiple daemons):**
- Plugin publishes a `service` record with their service metadata + node_id
- Daemon discovers the plugin via the service record (no Constellation backlink needed; service record IS the discovery surface)
- Daemon initiates atproto OAuth against the plugin's hosted identity provider (via jacquard-oauth)
- On consent, plugin emits T at the OAuth callback, scoped to the OAuth'd daemon's DID
- Daemon now holds T + expected_peer_did (from OAuth target) + peer_node_id (from service record)

### Proof construction

Each side constructs:

```
proof = encrypt_to(peer_node_pubkey, hmac(T, salt) || nonce || timestamp)
```

- `encrypt_to`: asymmetric encryption to the peer's iroh node_id pubkey (only peer's iroh secret key can decrypt). Iroh already uses ed25519; use the X25519 equivalent or a sealed-box construction.
- `hmac(T, salt)`: keyed HMAC over T with a fixed-constant salt encoded in the lexicon version. T being secret + HMAC's one-way property → attacker can't construct without T.
- `nonce`: per-pairing random nonce, prevents replay across pairings.
- `timestamp`: bounded validity, prevents replay across time.

### Record bodies

**`systems.atproto.plugin.peer`:**

```cbor
{
  "$type": "systems.atproto.plugin.peer",
  "peerDid": "<at://did:plc:...>",
  "myNodeId": "<base32 ed25519 node id>",
  "proof": "<bytes: encrypt_to(peer_node_pubkey, hmac(T,salt) || nonce || timestamp)>",
  "createdAt": "<iso8601>"
}
```

Both sides' peer records have the same shape. No request/accept asymmetry; the records are symmetric.

**`systems.atproto.plugin.service`:**

```cbor
{
  "$type": "systems.atproto.plugin.service",
  "nodeId": "<base32 ed25519 node id>",
  "name": "<plugin display name>",
  "version": "<semver>",
  "description": "<short text>",
  "capabilities": ["<wire capability identifiers>"],
  "oauthEndpoint": "<url for atproto oauth start>",
  "createdAt": "<iso8601>"
}
```

## Sequence (no advertise, self-flow with pre-shared node_id)

Simplest case — paste blob contains T + peer_did + peer_node_id:

1. User clicks 'pair' on side A. A generates T, displays paste-blob = `<T>:<A_did>:<A_node_id>:<nonce>`.
2. User pastes into side B's pairing CLI. B parses, stores `{ T, expected_peer_did=A_did, peer_node_id=A_node_id }`.
3. A also stores `{ T, expected_peer_did=B_did, peer_node_id=B_node_id }` (B's identity is shown to user during paste-back ceremony OR B initiates with its own paste-blob).
4. Either side publishes peer record first (race-tolerant). Say A goes first: `{ peerDid: B_did, myNodeId: A_node, proof: encrypt_to(B_node_pubkey, hmac(T,salt)||nonce||now()) }`.
5. B's observer (filtered by publisher_did==expected_peer_did=A_did) sees A's record, decrypts proof, verifies hmac matches B's T.
6. B publishes its own peer record with proof encrypted to A's node_id.
7. A's observer sees B's record, decrypts, verifies.
8. Both sides confirm pairing established. T is discarded; ceremony state cleared.

## Sequence (hosted-flow with service record)

1. Plugin operator publishes a `service` record at install/start: `{ nodeId, name, oauthEndpoint, ... }`.
2. Daemon admin runs `pattern plugin install at://<plugin-did>/systems.atproto.plugin.service/<rkey>` or similar; daemon fetches the service record.
3. Daemon opens the oauthEndpoint in a browser (or CLI flow), user logs in to plugin's identity provider via atproto OAuth.
4. Plugin's OAuth callback issues T scoped to daemon's DID (the OAuth-authenticated identity). Daemon stores `{ T, expected_peer_did=plugin_did, peer_node_id (from service record) }`.
5. Plugin stores `{ T, expected_peer_did=daemon_did, peer_node_id=... }` (peer_node_id may be unknown to plugin at this point — plugin queries Constellation for daemon's session record to retrieve it).
6. Both publish peer records as in the self-flow steps 4-7 above.
7. Pairing established. Optionally, after a configurable number of successful pairings or operator policy, the service record stays or is deleted.

## Security properties

**Defenses:**

- atproto record signature → identity-of-publisher (record is signed by claimed DID's PDS key; forgery requires PDS compromise = out of scope)
- Paired-record existence → bilateral-relationship-declared (both sides explicitly affirmed, observable via Constellation)
- Encrypted proof to peer's iroh node_pubkey → only legitimate peer can decrypt + verify
- HMAC over T inside the encrypted blob → attacker who can't decrypt can't reconstruct the value
- Out-of-band T (paste or OAuth) → can't pair without explicit human authorization
- Ephemeral T → no long-lived shared credential to leak
- Observer filter (publisher_did==expected_peer_did) → drops records from any DID we're not expecting, defense-in-depth
- Nonce + timestamp in proof → replay protection

**Threat: attacker M publishes fake peer record claiming peer_did=B**

- Even with arbitrary control of M's own PDS, M can't construct a valid proof without T.
- M's record reaches B's observer, but B can't decrypt (proof bytes are random-looking) — verification fails, record dropped.
- Defense-in-depth: B's observer filter rejects M's record by publisher_did mismatch before even attempting decryption.

**Threat: attacker observes A's published peer record and copies the proof bytes**

- Proof is `encrypt_to(B_node_pubkey, ...)` — attacker can't decrypt without B's iroh secret.
- If attacker copies the proof into a record claiming a different peer_did or publisher_did: B's observer filter drops it, OR B's HMAC verification fails (proof's expected peer-node-id doesn't match the publisher's claimed node_id).

**Threat: attacker compromises one side's iroh node_id secret**

- If A's iroh secret is leaked, attacker can decrypt past proofs and dial as A. This is iroh-level identity compromise — same threat model as any iroh-using system. Mitigation is keypair rotation (re-issue iroh keypair, re-pair). Detection is harder; consider adding periodic node_id rotation as a future enhancement.

**Out of scope (acceptable):**

- PDS-level compromise of either DID (attacker controls atproto identity entirely)
- Denial-of-service via Constellation flooding (atproto-layer concern)
- Side-channel attacks on the encryption primitives

## Tasks

### E.1 — Lexicon definitions

Define three lexicons (use jacquard-lexicon codegen for Rust types):
- `systems.atproto.plugin.session` (exists per Phase 7)
- `systems.atproto.plugin.service`
- `systems.atproto.plugin.peer`

Lexicons MUST be versioned via a numeric field so future ceremony-protocol changes are recognizable. Use `ceremonyVersion: u8` inside `peer` (and a matching constant in lexicon definitions).

### E.2 — Pairing ceremony state machine

New module: `pattern_runtime::plugin::pairing` (or similar). State:

```rust
pub struct PairingCeremony {
    pub token: Token,                     // T
    pub expected_peer_did: Did,
    pub peer_node_id: Option<NodeId>,     // may be None until session lookup
    pub nonce: [u8; 32],
    pub created_at: jiff::Timestamp,
    pub state: CeremonyState,
}

pub enum CeremonyState {
    Awaiting,                             // before we've published our peer record
    Published,                            // our record is up, waiting for peer's
    Verified(PairingGrant),               // both records exist and verified
    Failed(FailureReason),
}
```

Each in-progress ceremony has a unique key; ceremonies time out after N seconds (e.g. 5 min). Cleanup task drops expired ceremonies.

### E.3 — Self-flow CLI: `pattern plugin pair`

Subcommands:
- `pattern plugin pair init --peer-did <did>` — generates T + nonce, displays paste-blob, opens ceremony state
- `pattern plugin pair accept <paste-blob>` — parses paste-blob, opens ceremony state, publishes peer record
- `pattern plugin pair status` — list active ceremonies + their states
- `pattern plugin pair cancel <ceremony-id>` — abort an in-progress ceremony, delete published records if any

### E.4 — Hosted-flow OAuth integration

- Plugin SDK helper: `pattern_plugin_sdk::serve_oauth_pairing(callback_url)` — drops a webserver on the plugin's machine listening for OAuth callbacks; on consent, mints T + opens a ceremony on the plugin side
- Daemon-side: `pattern plugin install <at-uri>` — fetches service record, opens OAuth URL in browser (or CLI flow), receives T at completion, opens ceremony on daemon side
- Uses jacquard-oauth for atproto OAuth client + server flows

### E.5 — Peer record verification + publish

- Verify-publish helper that constructs proof + publishes via jacquard.put_record
- Observer task (one per active ceremony) that polls Constellation backlinks every N seconds for peer records targeting our DID
- On receiving a candidate peer record: filter by publisher_did == expected_peer_did, attempt decrypt+HMAC verify, transition state on success

### E.6 — Connection-time pair-record validation

- Modify `OutOfProcessPluginConnection::connect` for remote plugins: before dialing, verify both peer records exist via Constellation. If either is missing → reject.
- Cache verification results with a polling interval (e.g. re-validate every 5 min during active session); on revocation detection, drop the connection.
- Per the existing Phase A allowlist work: remote-plugin pubkeys go into the same `PluginRouteTable` as local plugin pubkeys, but with a `RemoteAuth::Atproto { peer_did, last_verified }` tag tracking the pairing.

### E.7 — Service record (optional, hosted-flow only)

- Plugin SDK helper for publishing service records on plugin start (`pattern_plugin_sdk::publish_service_record(...)`)
- Daemon-side discovery: `pattern plugin discover <plugin-did>` — fetches the service record, displays metadata for user confirmation, offers to initiate OAuth pairing

### E.8 — Revocation paths

- `pattern plugin revoke <plugin-id>` — daemon deletes its peer record. Future dial attempts will fail at the validation step.
- Plugin-side: equivalent CLI subcommand for the plugin operator. (Hosted plugins likely want a UI for this; CLI is fallback.)
- Both sides should detect peer-record-deletion via Constellation polling and tear down active connections cleanly.

## Acceptance criteria

- Self-flow: `pattern plugin pair init` + paste into peer machine + `pattern plugin pair accept` → both peer records published, both verified, dial succeeds
- Hosted-flow: plugin publishes service record, daemon discovers + OAuths + pairs, dial succeeds
- Revocation: either side deleting their peer record breaks future dials within polling-interval
- Attacker forgery: peer record with valid signature but invalid proof is rejected; peer record with wrong publisher_did is filtered before processing
- Replay: copying a valid peer record's proof bytes into a new record fails verification (nonce + publisher mismatch)
- Workspace tests pass

## Gotchas

- **Iroh asymmetric encryption to ed25519 pubkey**: ed25519 is a signing key; encryption uses X25519. Iroh's keypair derivation should give us both; verify the API path for sealed-box encryption to a node's pubkey. If not directly supported, derive X25519 from the ed25519 secret via the standard transformation.
- **Constellation polling latency**: backlinks can take seconds-to-minutes to propagate. Acceptance test for revocation must allow the polling interval (e.g. 5 min) before asserting connection-drop.
- **Lexicon NSID stability**: once we publish records, we can't trivially rename the NSID. Pick `systems.atproto.plugin.{session,service,peer}` deliberately + version internally via `ceremonyVersion` field.
- **OAuth state binding**: the T issued at OAuth callback MUST be scoped to the OAuth-authenticated daemon's DID. Don't let one daemon's OAuth consume another daemon's pending ceremony.
- **Self-pairing where both sides are same DID** (e.g. user pairs their plugin + daemon under their own atproto identity): `publisher_did == expected_peer_did` filter still works because the filter is on EACH side independently. Both sides expect the same DID; both records signed by same DID; both pass the filter; both verify proofs. Fine.
- **Don't paint over the existing v3-extensibility Phase 7 work.** Phase 7's session record + Constellation client is what we build on. Phase E is an extension/refinement of Phase 7's atproto-auth shape, not a replacement.
- **Replay protection nonce is per-ceremony, not per-message.** Once a peer record is published with a specific (nonce, timestamp), don't republish with the same nonce. Generate fresh nonce on each ceremony invocation.

## Out of scope

- Cross-mount pairing (one Pattern mount paired with another Pattern mount — separate concern, different threat model)
- Multi-tenancy / quorum signing (multiple operators co-managing one plugin)
- Plugin update through atproto records (just provides discovery + auth; update flow is a different concern)
- Persistent shared secrets (rejected — ephemeral T is sufficient given the peer-record gating)

## Verification before declaring done

1. Spin up two Pattern instances on different machines. Pair them via self-flow. Plugin operations work.
2. Delete the daemon-side peer record on instance B. After polling interval, instance A's pairing connection drops cleanly.
3. Re-pair (fresh T, fresh ceremony). Connection re-establishes.
4. Hosted-flow: minimal test plugin publishes service record; mock-OAuth flow issues T; pairing succeeds end-to-end.
5. Try to forge a peer record from a third DID (different keypair, different machine): all verification paths reject it.
6. Try to replay a valid peer record after a ceremony completes (re-publish same proof bytes): rejected at nonce / timestamp / publisher_did mismatch.
7. Workspace test suite + Phase 7's smoke test still passes.
