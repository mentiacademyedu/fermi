# F-PLUGIN

Sample plugins for the plugin tests (PL-01 … PL-08). Each folder is a plugin:
a `plugin.toml` whose program is the harness's `fake-plugin` (an MCP server
over stdio, `crates/acceptance-tests/src/bin/fake-plugin.rs`). The tests copy a
folder and fill in `{FAKE_PLUGIN}` (that program's path) and the other
`{PLACEHOLDERS}` (marker, log, sentinel and script files in the case's folder).
Plugins run sandboxed, so the files a plugin writes or reads for a test live in
`{PROBE}`, a folder its manifest grants with `read_write`; the PL-07 sentinel
lives elsewhere, outside every grant.

| Plugin | Used by | What it is |
| --- | --- | --- |
| sample 1.2.0 | PL-01, PL-02 | declares a tool (`echo`) and a command (`greet`); also offers `add`, which it doesn't declare |
| other 0.3.1 | PL-02, PL-04 | an unrelated plugin: command `sum` (its `add` tool) |
| alpha, beta | PL-03 | both claim the command `greet` |
| faulty 0.9.0 | PL-04 | `boom` panics, `hang` never answers, `fail` reports a tool error; 3 s timeout |
| adapter 1.0.0 | PL-06 | a model adapter (`scripted`) that plays scripted replies |
| adversarial 0.1.0 | PL-07 | tries to read a sentinel outside its scope, directly and through a path argument |
| future 3.0.0 | PL-08 | needs plugin API 2, which this fermi doesn't support |
