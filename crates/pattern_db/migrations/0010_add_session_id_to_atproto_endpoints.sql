-- Add session_id column to agent_atproto_endpoints
-- Allows agents to use agent-specific sessions with fallback to "_constellation_"
ALTER TABLE agent_atproto_endpoints ADD COLUMN session_id TEXT;

-- Create index for session_id lookups
CREATE INDEX idx_agent_atproto_endpoints_session ON agent_atproto_endpoints(session_id);
