# odori-mcp-bridge

`odori-mcp-bridge` exposes framework-owned tools to subscription harnesses as
an authenticated, loopback MCP server. Accepted calls become durable
activities, so tool results replay from workflow history rather than executing
again.

The bridge is a preview feature and is inert unless `preview` is enabled. See
the [durable tools guide](https://github.com/tokeira/tokeira-odori/blob/main/docs/durable-tools.md)
for its attachment and at-most-once semantics.

Licensed under Apache-2.0.
