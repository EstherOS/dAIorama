# Input tools — live leg (P5, act half)

The nine input tools shipped as an in-binary Wayland client (`src/runtime/input.rs`): one
short-lived connection per call, `zwlr_virtual_pointer_manager_v1` for the pointer,
`zwp_virtual_keyboard_manager_v1` for keys, a built-in US keymap uploaded per connection.
The unit tests cover the keysym table and the refusals; nothing exercised the path against a
real compositor, so this leg does: a real `daiorama serve`, a real mutter→Hyprland session,
and the nine tools driven through the shipped harness entry (`daiorama mcp`, stdio, scope in
`DAIORAMA_SCOPE`).

The observation is the client's own state, never the tool's return value:

- pointer — `hyprctl cursorpos` in the session's own instance (screenshot pixels at scale 1,
  and 300,200 screenshot pixels = 150,100 logical at scale 2);
- keyboard — a `foot` window running `cat` on a raw tty (`stty -icanon min 1 -echo`), so every
  delivered byte lands in a file. Compared byte-exactly with `cmp`; shell command substitution
  strips trailing newlines, which is exactly what a Return tap delivers.

## What this leg found (three defects, all invisible to the unit tests)

Measured on the merged tip `f69e3c9` and on the fixed tree. Pre-fix and post-fix runs are the
same script with `DAIO=<binary>`; the only difference is the binary.

1. **The first key of every call was dropped.** A virtual keyboard's keymap is applied
   asynchronously by the compositor, so a key sent in the same dispatch as the keymap was
   translated against the not-yet-installed map and silently discarded. Pre-fix, every
   `type_text "abcdef"` delivered `bcdef` (2/2 calls) and a one-pair `key_tap Return` delivered
   nothing at all (2/2). Fixed by a settle roundtrip after the keymap upload.
2. **Shift was never applied.** A client translates a key with the mask of the last
   `wl_keyboard.modifiers` event it received; the virtual-keyboard protocol carries a
   `modifiers` request for exactly that, and the code never sent it. Pre-fix, `type_text "ABC!"`
   delivered `abc1` and `key_tap X` delivered `x`; fixed, both arrive shifted. Attaching the
   mask to the Shift key press (at 30 ms and 200 ms settles) did **not** fix it — the request is
   required, not a delay.
3. **One-character keysym names were refused.** 05 names `a` beside `Return`, and
   `xkb_keysym_from_name("a")` is 0x61, but the table only held named keys: `key_tap "a"`
   answered `bad_request: unknown key "a"`. Fixed by resolving a one-character name through the
   same US table.

## Chord follow-up (P5-INPUT-CHORD)

The measured gap — `key_down Shift_L` → `key_tap x` → `key_up Shift_L` delivering `x` — is fixed
by manager-side modifier state rather than a device that outlives the call: `key_down`/`key_up`
record the held modifiers per session, and the next tap/text assembles them inside its own
connection (05). `probe-chord.sh` now expects `CHORD=works`; it was measured on the merged tip
`09c6683` in the L1 image (`p2/hardening/evidence/input-surface-09c6683.log` — `CHORD=works`,
`SHIFTED_TAP=works`), together with `probe-up-symmetry.sh` for the refusal arm.

## Running it

```
# build the tree under test (and, for the before/after evidence, the pre-fix tip)
cargo build --locked --bin daiorama
cp target/debug/daiorama /out/daiorama-input

# L1 image, privileged, /dev/dri, repo at /src (ro), binaries at /out
dbus-run-session -- bash p5/input/live/run-input-live.sh     # the leg, FAILS=<n>
dbus-run-session -- bash p5/input/live/probe-firstkey.sh     # defect 1, either binary
dbus-run-session -- bash p5/input/live/probe-shift.sh        # defect 2, either binary
dbus-run-session -- bash p5/input/live/probe-chord.sh        # the chord across calls
dbus-run-session -- bash p5/input/live/probe-up-symmetry.sh  # modifier refusal symmetry
```

`probe-input-live.py <binary> <scope> <tool> <json-args>` is the MCP client: it spawns
`daiorama mcp`, runs the legacy `initialize` handshake and one `tools/call`, and prints the
result. A scope file must grant the categories it calls (`act.ron` in the scripts grants
`input`/`capture`/`query`).

## Evidence

`evidence/input-live-f69e3c9.log` — one run of all six phases in a fresh container at the fixed
tree: 21 assertions, 0 failures on the full leg, and the pre-fix red preserved for both defects
1 and 2 rather than re-described.
