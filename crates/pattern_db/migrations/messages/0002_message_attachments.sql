-- Round-trip turn-level metadata (MessageAttachment Vec + MessageOrigin)
-- across process restart.
--
-- Both fields are pattern-level metadata not previously persisted:
--
-- attachments_json: pattern-level MessageAttachment values that render onto
-- the wire at compose-time but live separately from the stored ChatMessage.
-- Examples: BatchOpeningSnapshot (memory snapshot), SkillAvailable (plugin
-- auto-install notification), Custom (caller-rendered fragment).
-- Pre-this-migration, db_message_to_core defaulted attachments to
-- Vec::new() on restore — every attachment ever attached was lost on process
-- restart. This regressed the "write-once, never updated" attachment
-- contract that agent_loop's splice machinery relies on for cache stability.
-- The column stores a JSON array of MessageAttachment values verbatim
-- (serde Vec<MessageAttachment>). NULL is equivalent to an empty Vec.
--
-- origin_json: TurnInput.origin (provenance: who authored the messages and
-- into what visibility sphere). Pre-this-migration, restore_turns_from_db
-- inferred origin lossy from `batch_type` via `infer_origin_from_batch_type`
-- — the four-shape mapping from BatchType to MessageOrigin lost any caller
-- detail (specific user_id, source channel, etc.). The column stores the
-- original MessageOrigin verbatim (serde JSON). Stored redundantly on every
-- message of a turn so single-message queries keep origin context; the
-- restore path uses the first message's origin per batch as the turn's
-- origin. NULL falls back to the legacy batch_type inference for
-- pre-migration rows.

ALTER TABLE messages ADD COLUMN attachments_json TEXT;
ALTER TABLE messages ADD COLUMN origin_json TEXT;
