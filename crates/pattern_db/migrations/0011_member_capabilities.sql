-- Add capabilities column to group_members table
-- Capabilities are stored as a JSON array of strings
ALTER TABLE group_members ADD COLUMN capabilities JSON DEFAULT '[]';
