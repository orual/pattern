-- Copyright 2026 Pattern contributors
--
-- This Source Code Form is subject to the terms of the Mozilla Public
-- License, v. 2.0. If a copy of the MPL was not distributed with this
-- file, you can obtain one at http://mozilla.org/MPL/2.0/.

{-# LANGUAGE GADTs #-}
-- | Pattern.Tasks — task-graph operations.
--
-- 'Tasks' uses a @BlockHandle@+@TaskItemId@ pair (serialised as
-- @\"handle#item\"@ in a 'TaskEdgeRef') to address individual task items.
-- This design keeps addressing self-contained in a single 'Text' field,
-- which composes cleanly with Haskell pattern-matching and avoids
-- introducing a two-field constructor for every method.
--
-- JSON-encoded payloads ('TaskSpec', 'TaskCreateRequest', 'TaskPatch',
-- 'TaskStatus', 'TaskFilter', 'GraphQuery') are passed as opaque 'Text'
-- blobs.  The
-- runtime decodes them on the Rust side; agents that want typed
-- construction should use the helpers below or build the JSON via
-- @Pattern.Aeson@.
--
-- This module is always imported qualified:
--
-- > import qualified Pattern.Tasks as Tasks
-- > Tasks.create block requestJson  -- requestJson :: TaskCreateRequest
--
-- 'List' is named as-is (no underscore suffix needed) because the
-- qualified import prevents collision with 'Prelude.list' or other
-- effect modules' 'List' constructors.  'QueryGraph' maps to @Tasks.queryGraph@.
module Pattern.Tasks where

import Control.Monad.Freer (Eff, Member, send)
import Data.Text (Text)

-- | Opaque handle identifying a memory block (same type as in
-- 'Pattern.Memory').
type BlockHandle = Text

-- | Identifier for a task item within a TaskList block.
type TaskItemId = Text

-- | Serialised edge reference of the form @\"block-handle#item-id\"@.
-- Both the source and target of link\/unlink operations are expressed
-- as 'TaskEdgeRef' values.
type TaskEdgeRef = Text

-- | JSON-encoded task specification. Build with @Pattern.Aeson@ or pass
-- a pre-encoded 'Text' literal. Schema:
--
-- > {
-- >   "subject": Text,              -- required
-- >   "description": Text,          -- required (use "" for none)
-- >   "status": TaskStatus?,        -- optional; defaults to "pending"
-- >   "owner": AgentId?,            -- optional
-- >   "active_form": Text?,         -- optional; "what is currently happening"
-- >   "metadata": Value             -- required; use null for none
-- > }
type TaskSpec = Text

-- | JSON-encoded request payload for 'Create'. A single 'Create' call can
-- seed a fresh TaskList block with N items in one shot, optionally setting
-- the block's description at the same time. Schema:
--
-- > {
-- >   "block_description": Text?,   -- optional; describes the *block*,
-- >                                 -- applied only when this call auto-
-- >                                 -- creates the block. Ignored when the
-- >                                 -- target block already exists.
-- >   "items": [TaskSpec]           -- required; non-empty list of items
-- >                                 -- to add. Ids returned in input order.
-- > }
--
-- The Create call returns a JSON-encoded array of 'TaskItemId' strings, in
-- the same order as @items@. Use @Pattern.Aeson@ to decode if you need to
-- bind individual ids; for fire-and-forget creates the array text is fine
-- to log/display as-is.
type TaskCreateRequest = Text

-- | JSON-encoded task patch. Only provided fields are updated; omitted
-- fields are left untouched. Schema:
--
-- > {
-- >   "subject": Text?,             -- optional set-or-leave
-- >   "description": Text?,         -- optional set-or-leave
-- >   "status": TaskStatus?,        -- optional set-or-leave
-- >   "metadata": Value?,           -- optional set-or-leave
-- >   "owner": AgentId|null??,      -- absent=leave, null=clear, value=set (double-option)
-- >   "active_form": Text|null??    -- absent=leave, null=clear, value=set (double-option)
-- > }
type TaskPatch = Text

-- | JSON-encoded task status — a bare JSON string in kebab-case. One of:
-- @\"pending\"@, @\"in-progress\"@, @\"blocked\"@, @\"completed\"@,
-- @\"cancelled\"@. Serialized with surrounding quotes, e.g.
-- @\"\\\"in-progress\\\"\"@.
type TaskStatus = Text

-- | JSON-encoded task filter. Use @\"{}\"@ for an unfiltered list.
-- Schema:
--
-- > {
-- >   "status": [TaskStatus]?,     -- optional; any of these statuses
-- >   "owner": AgentId?,           -- optional; owned by this agent
-- >   "has_blockers": Bool?,       -- optional; has/has-not incoming edges
-- >   "keyword": Text?,            -- optional; FTS5 MATCH expression
-- >   "blocks": [BlockHandle]?     -- optional; restrict to these blocks
-- > }
type TaskFilter = Text

-- | JSON-encoded 'TaskView' record returned as an element of the 'List'
-- result. Each 'TaskView' is opaque 'Text'; agents decode individually
-- via @Pattern.Aeson@ (lens accessors — no @FromJSON@ yet). Schema:
--
-- > {
-- >   "block_ref": TaskEdgeRef,
-- >   "subject": Text,
-- >   "status": TaskStatus,
-- >   "owner": AgentId?,
-- >   "blocker_count": Int,        -- tasks that block this one (in-degree)
-- >   "blocks_count": Int          -- tasks this one blocks (out-degree)
-- > }
--
-- A follow-up task ("typed SDK records via vendored JSON parser") will
-- replace these opaque Texts with real Haskell records + FromJSON.
type TaskView = Text

-- | JSON-encoded graph-query parameters. Schema:
--
-- > {
-- >   "direction": Direction,       -- "forward", "reverse", or "both"
-- >   "depth": Int?,                -- optional; default 16
-- >   "max_nodes": Int?             -- optional; default 1000
-- > }
type GraphQuery = Text

-- | JSON-encoded graph slice returned by 'QueryGraph'. Schema:
--
-- > {
-- >   "nodes": [TaskEdgeRef],       -- all discovered nodes
-- >   "edges": [[TaskEdgeRef, TaskEdgeRef]],  -- (source, target) pairs
-- >   "truncated": Bool             -- true if depth/max_nodes cap was hit
-- > }
--
-- Edges are always in source→target orientation regardless of traversal
-- direction (Reverse traversal returns edges pointing at the root, not
-- outward from it).
type GraphSlice = Text

-- | Task effect algebra.
--
-- Constructor names match the Rust 'TasksReq' variants exactly so that
-- the @#[core(module = \"Pattern.Tasks\", name = \"...\")]@ derive
-- attributes decode them without manual mapping.
--
-- 'Create' returns a JSON-encoded array of 'TaskItemId' strings (one
-- per item in the request).  The other constructors return either unit
-- or a structured payload as documented per-constructor.
data Tasks a where
  Create     :: BlockHandle  -> TaskCreateRequest -> Tasks Text
  -- ^ Create one or more task items in the given block, optionally
  -- auto-creating the TaskList block (with 'block_description' from
  -- the request) if it doesn't exist yet.  Returns a JSON-encoded
  -- array of 'TaskItemId' strings, in the same order as @items@.
  -- Decode with @Pattern.Aeson@ if you need to bind individual ids.
  Update     :: TaskEdgeRef  -> TaskPatch  -> Tasks ()
  -- ^ Apply a partial patch to an existing task.  Unspecified fields
  -- are left unchanged.  Returns 'MemoryError::TaskNotFound' if the
  -- ref does not exist.
  Transition :: TaskEdgeRef  -> TaskStatus -> Tasks ()
  -- ^ Transition a task to a new status.  If transitioning to
  -- @Completed@, the runtime records a @completed_at@ timestamp in
  -- the task's loro map.
  Link       :: TaskEdgeRef  -> TaskEdgeRef -> Tasks ()
  -- ^ Add a directed dependency edge: source depends on target.
  -- Only the source block's LoroDoc is modified (single-source-of-truth
  -- edge model).
  Unlink     :: TaskEdgeRef  -> TaskEdgeRef -> Tasks ()
  -- ^ Remove a directed dependency edge.  No-op if the edge does not
  -- exist.
  List       :: Maybe BlockHandle -> TaskFilter -> Tasks [TaskView]
  -- ^ List tasks.  Pass 'Nothing' to enumerate tasks across all
  -- scope-visible TaskList blocks.  Returns a list of JSON-encoded
  -- 'TaskView' records.
  QueryGraph :: TaskEdgeRef  -> GraphQuery -> Tasks GraphSlice
  -- ^ BFS traversal of the task dependency graph from the given root.
  -- Returns a JSON-encoded 'GraphSlice'.
  AddComment :: TaskEdgeRef  -> Text       -> Tasks ()
  -- ^ Append a comment to a task.  The runtime attaches the current
  -- agent's id and a timestamp automatically.

-- | Create one or more task items in @block@.  @request@ is a
-- JSON-encoded 'TaskCreateRequest' (@{block_description?, items: [TaskSpec]}@);
-- the block is auto-created as a TaskList if it doesn't exist, applying
-- @block_description@ at that point.
--
-- Returns a JSON-encoded array of 'TaskItemId' strings in input order.
-- Use @Pattern.Aeson@ to decode if you need individual ids:
--
-- > ids <- Tasks.create "my-tasks" requestJson
-- > -- ids :: Text — e.g. \"[\\\"01HXX...\\\",\\\"01HXY...\\\"]\"
create :: Member Tasks effs => BlockHandle -> TaskCreateRequest -> Eff effs Text
create block request = send (Create block request)

-- | Apply a partial patch to the task addressed by @ref@.
--
-- Only fields present in the JSON patch are updated.
update :: Member Tasks effs => TaskEdgeRef -> TaskPatch -> Eff effs ()
update ref patch = send (Update ref patch)

-- | Transition the task addressed by @ref@ to a new status.
transition :: Member Tasks effs => TaskEdgeRef -> TaskStatus -> Eff effs ()
transition ref status = send (Transition ref status)

-- | Add a directed dependency edge from @src@ to @tgt@.
link :: Member Tasks effs => TaskEdgeRef -> TaskEdgeRef -> Eff effs ()
link src tgt = send (Link src tgt)

-- | Remove the directed dependency edge from @src@ to @tgt@.
unlink :: Member Tasks effs => TaskEdgeRef -> TaskEdgeRef -> Eff effs ()
unlink src tgt = send (Unlink src tgt)

-- | List tasks.  Pass 'Nothing' for @block@ to search all
-- scope-visible TaskList blocks.  Pass @\"{}\"@ for @filter@ to return
-- all tasks without filtering.
list :: Member Tasks effs => Maybe BlockHandle -> TaskFilter -> Eff effs [TaskView]
list block filt = send (List block filt)

-- | BFS traversal of the task dependency graph starting from @root@.
queryGraph :: Member Tasks effs => TaskEdgeRef -> GraphQuery -> Eff effs GraphSlice
queryGraph root query = send (QueryGraph root query)

-- | Append a plain-text comment to the task addressed by @ref@.
addComment :: Member Tasks effs => TaskEdgeRef -> Text -> Eff effs ()
addComment ref txt = send (AddComment ref txt)
