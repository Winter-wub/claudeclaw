# Hand-made stream-json fixtures

Small transcripts shaped like Claude Code 2.1.x `--print --output-format stream-json --verbose`
output, written by hand from the shapes recorded in #5, #39 and #42 and the TS daemon's
handling in `src/runner.ts`. Every ID is fake. `@CONFIG@` is replaced at test time with an
absolute temp path, so `memory_paths.auto` passes the shape check on every OS.

They stand in until #18's captured F3 transcripts land under `parity/fixtures/`.
