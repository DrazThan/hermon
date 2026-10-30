# Vitest report stream

Some applications write diagnostics directly to stdout, which can precede the
JSON reporter document. Console silencing does not isolate direct stream writes.
Strict VitestJson parsing must reject that combined stream.

The example invokes `pnpm exec /bin/sh -c` with this body, retaining pnpm's
executable environment and the installed Vitest CLI:

```sh
exec 3>&1
exec 1>&2
exec node node_modules/vitest/vitest.mjs run --workspace tests/vitest.workspace.ts --no-file-parallelism --reporter=json --outputFile=/dev/fd/3
```

Descriptor 3 retains report output while ordinary test stdout goes to captured
diagnostic stderr. Public Vitest outputFile writes the complete report through
descriptor 3. Exec preserves the actual exit status and process-group ownership.

Two isolated synthetic fixtures wrote stdout diagnostics and asserted a passing
or failing condition. Complete stdout parsed as one JSON document, diagnostics
remained on stderr, and exits were 0 and 1 respectively. Their receipt is
`report-stream-fixture.json`. A representative database-backed integration test
also passed; `report-stream-real-summary.json` retains only its aggregate counts.
These bounded checks do not replace full project preflight.
