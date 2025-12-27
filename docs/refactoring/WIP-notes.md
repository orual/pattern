notes:

Auth/Identity
- identity and auth, we need to decide whether to scope to constellation db or put in separate sqlite db.
- keep in same is simpler, scopes, means no 'user id' really needed
- but separation means can be reused between constellations, constellation db doesn't contain any tokens, etc.

Constellation Activity
- need to start up/configure in cli still, add option to config file

Model + Embedding Providers
- need to improve loading/configuration flow, re-wire up in cli with better configuration
- static keys still in env variable, but load more cleanly
- oauth option for anthropic needs to be wired back up

--- later ---
Data sources and memory:
- bluesky data source should be reworked to use ~/Projects/jacquard, this will be much simpler

plan larger rework of data sources in line with below:
docs/refactoring/v2-memory-system.md
docs/refactoring/v2-overview.md
