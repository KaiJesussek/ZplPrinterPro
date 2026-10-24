# HTTP API

`zpl-server` serves a JSON API and a web UI on the same port (`ZPL_HTTP_PORT`,
default `8080`). API routes live under `/api/*`; anything unmatched falls
through to the static UI, so the UI is at `/` and never shadows an endpoint.
The raw ZPL session port (`ZPL_TCP_PORT`, default `9100`) is separate and not
part of this API.

All requests and responses are JSON unless noted (PNG / raw bytes).

## Errors

Errors are a JSON body `{"error": "..."}` with an HTTP status:

| Status | Meaning |
| --- | --- |
| `400` | Bad input: malformed JSON, unknown enum string, `limit=0`, `dpi=0`, an incomplete `^XA...^XZ` format, ... |
| `404` | No such label id |
| `500` | Server-side inconsistency (e.g. corrupt archive file)  --  never a panic on hostile input |

## Endpoints

| Method & path | Returns | Notes |
| --- | --- | --- |
| `GET /api/labels?limit=&since=` | `[LabelRow]`, newest-first | `limit` caps the count, `since` a `ts_ms` floor; both optional |
| `GET /api/labels/{id}` | `{"meta": LabelRow, "ir": Label, "diags": [msg]}` | parsed IR + human-readable diagnostics |
| `GET /api/labels/{id}.zpl` | raw ZPL (`text/plain`) | the exact archived bytes |
| `GET /api/labels/{id}.png` | PNG (`image/png`) | the cached render |
| `POST /api/labels/{id}/rerender?dpi=` | `{"id","dpi","width","height"}` | re-parse the stored raw, re-render at `dpi`, rewrite the PNG |
| `POST /api/render?dpi=` | PNG inline (`image/png`) | direct ZPL in the body, not archived; `x-label-width`/`x-label-height` headers |
| `POST /api/labels` | `[LabelRow]` JSON (newest last) | print ZPL in the body through the TCP session path (render + archive + SSE); 400 if no complete format |
| `GET /api/printer-state` | `Conditions` | the host's view of the printer |
| `PUT /api/printer-state` | merged `Conditions` | **partial patch**  --  see below |
| `GET /api/storage` | `{images, fonts, formats, counts}` | sizes, never image pixel data |
| `GET /api/diagnostics` | `{labels, bytes, warnings, unknown_commands, prunes}` | boot counters |
| `GET /api/events` | SSE stream | `event: label` (a `LabelRow`) and `event: state` (a `Conditions`) |

A `LabelRow` is `{id, ts_ms, bytes, diags, width, height}`  --  `diags` is the
message count at archive time (the full messages come from `GET /api/labels/{id}`).

`/api/render` (the UI playground) and `/api/labels/{id}/rerender` both resolve
stored images (`^XG`/`^IS`) against the current shared printer memory, exactly
like print time.

## `PUT /api/printer-state`  --  setting the individual printer flags

This is the way to drive the emulated printer's hardware conditions  --  the
handy seam for tests of the `~HS` host-status behavior (including its
suppression rules and per-bit mapping). The body is a **partial patch**: only
the keys you send are changed, the rest keep their current value, and the
response is the merged `Conditions`.

| JSON key | Type | `~HS` effect |
| --- | --- | --- |
| `paper_out` | `bool` | suppression  --  no `~HS` response when set |
| `ribbon_out` | `bool` | suppression |
| `head_open` | `bool` | suppression |
| `rewind_full` | `bool` | suppression |
| `head_temp` | `"Normal"` \| `"Warm"` \| `"OverTemp"` | `"OverTemp"` suppresses; `"Warm"` sets the delivered `l` (temperature) bit |
| `pause` | `bool` | delivered `c` bit |
| `comm_diag` | `bool` | delivered `g` bit and `mmm` bit 5 |
| `media_type` | `"DieCut"` \| `"Continuous"` | `mmm` bit 7 |
| `print_mode` | `"DirectThermal"` \| `"ThermalTransfer"` | `mmm` bit 0 and the `q` bit |
| `static_ram` | `bool` | delivered string-3 `y` |

`GET /api/printer-state` reads the same values back. An unknown enum string
(e.g. `"Frozen"`) is a `400`; so is malformed JSON.

The TCP `~HS` responder reads the **same** `PrinterState` the endpoint
mutates, so a `PUT` takes effect immediately on the next `~HS`  --  on any open
or new socket, no restart. `tests/proto.rs`
(`hs_suppressed_while_media_out_via_put`) and `tests/http.rs` pin exactly this
end-to-end.

Set paper-out and watch `~HS` go silent, then recover:

```sh
# suppress: ~HS now returns NO bytes (guide p.233 media-out condition)
curl -s -X PUT -H 'content-type: application/json' \
  -d '{"paper_out":true}' \
  http://localhost:8080/api/printer-state
#   -> {"paper_out":true,"ribbon_out":false,"head_open":false,...}

# recover: ~HS answers again with <STX>...<ETX> frames
curl -s -X PUT -H 'content-type: application/json' \
  -d '{"paper_out":false}' \
  http://localhost:8080/api/printer-state
```

The same one-liner drives the other suppression flags and the temperature
ladder:

```sh
# over-temperature (suppresses), then back to a warm head (delivered l=1)
curl -s -X PUT -d '{"head_temp":"OverTemp"}' http://localhost:8080/api/printer-state
curl -s -X PUT -d '{"head_temp":"Warm"}'     http://localhost:8080/api/printer-state
```

One boundary worth keeping straight: these are **server-side emulator state**
(the host's view of the printer), not ZPL commands  --  the `zpl` crate itself has
no concept of them. `PUT /api/printer-state` is therefore the right knob for
testing `zpl-server`'s `~HS` behavior; there is no equivalent on the `zpl`
parse/render side.
