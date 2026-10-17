# `--listen` adversarial HTTP battery

`probe-http.py` runs a real manager (off-mode) plus a real `daiorama mcp --listen` and speaks
raw HTTP/1.1 over sockets. It exists because the network transport has shapes a handcrafted
JSON-RPC client never produces: header duplication, whitespace before a colon, `Content-Length`
forms a proxy reads differently, path forms, scope names, and connection lifetimes.

    python3 p3/listen/evidence/probe-http.py            # against target/debug/daiorama

`probe-http-before.txt` is the same battery against the merged W1b tree (`7c9d11c`) before the
strict-head fix; `probe-http.txt` is the fixed tree. The four differences are the finding:

| case | before | after |
|---|---|---|
| duplicate `Host` (two values, or the same twice) | 200, last value wins | 400 `malformed HTTP request` |
| empty `Host` | 200 (passed the "Host required" check) | 400 `malformed HTTP request` |
| `Host : value` (space before the colon) | 200, name trimmed | 400 `malformed HTTP request` |
| `Content-Length: +N` | 200, body framed | 400 `invalid Content-Length: "+N"` |

The last section of the run is exploratory (`WATCH`): bare-LF heads (accepted, RFC 7230 allows
it), leading-zero lengths (accepted, they are digits), scope-name traversal (all answer
`scope_unknown` — `load_scope` runs first, no file is read), origin shapes, pipelining, and the
connection-lifetime gap: 64 idle connections stay open, a slow body and an idle head are held
with no timeout. That last item is tracked in `STATUS.md` as an open item, not fixed here —
a timeout needs a pinned value.
