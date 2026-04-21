// This file must FAIL to compile. If it compiles, `pattern_core` has
// gained a dependency on `pattern_memory`, which violates the layering
// invariant: pattern_memory depends on pattern_core, never the reverse.

use pattern_memory::MemoryCache;

fn main() {}
