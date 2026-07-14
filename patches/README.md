# Vendored dependency patches

We carry small patches to `egui-snarl` because parts of its interaction model are
hardcoded and not configurable through its public API. `egui-snarl` is vendored in
`vendor/egui-snarl/` and the workspace points at it via `[patch.crates-io]` in the root
`Cargo.toml`. Each patch below is also kept as a unified diff against the pristine crate
source, so the changes are reviewable and re-appliable on an upgrade.

The patches are independent and touch different regions of `src/ui.rs` and
`src/ui/viewer.rs`; applied in order (the five below) they reproduce the vendored source
exactly.

## 1. `egui-snarl-middle-pan.patch`

snarl's embedded `Scene` is hardcoded to pan on left-drag, and that drag is not
configurable. We need left-drag on the empty canvas free for the app's own marquee
box-select (#84), with panning on the middle mouse button. The single change moves the
Scene's drag-pan to the middle button:

```rust
Scene::new()
    .zoom_range(min_scale..=max_scale)
    .drag_pan_buttons(DragPanButtons::MIDDLE) // the patch
    .register_pan_and_zoom(&ui, &mut snarl_resp, &mut to_global);
```

The proper long-term fix is to make the pan button a `SnarlStyle` option upstream.

## 2. `egui-snarl-click-to-wire.patch`

snarl wires pins by drag only: a primary drag from a pin arms a wire, drag-release over
a compatible pin completes it. A plain click on a pin does nothing, so reliably wiring
requires a precise press-hold-drag-release, which is easy to miss (#50). This patch adds
**click-to-wire** alongside the existing drag, reusing snarl's own pending-wire state and
rubber-band rendering (which already persist across frames and follow the cursor):

- A primary click on a pin arms a wire from it (mirroring what `drag_started` does).
- A primary click on a compatible opposite pin completes the connection, routed through
  `viewer.connect` so core stays the validity authority exactly as for a dragged wire.
- `Esc`, a right-click, or a primary click on empty canvas cancels the armed wire. The
  empty-canvas cancel is guarded on `pin_hovered` so the click that arms or completes a
  wire over a pin can never also cancel it in the same frame.
- A new `SnarlViewer::on_wire_click` hook (default no-op) is called when a pin click
  begins or completes a wire, so the host can suppress its own handling of that same
  click (Ymir uses it to avoid selecting the node under the pin).

The change is four small pieces: in `src/ui.rs` the input-pin click handler, the
output-pin click handler (its mirror), and the cancel block; in `src/ui/viewer.rs` the
`on_wire_click` trait method. The proper long-term fix is for snarl to support
click-to-wire (or expose pin rects and a hook so the host can) upstream.

## 3. `egui-snarl-output-pin-space.patch`

Output port labels rendered jammed under their pins, while input labels had a clean gap
(#55). The cause is an asymmetry in how snarl reserves the pin slot: input rows are
left-to-right and output rows right-to-left, but both reserve the slot the same way —
`advance_cursor_after_rect(Rect::from_min_size(next_widget_position(), (spacing, size)))`.
In a left-to-right row `next_widget_position` is the left edge and the rect extends right,
*into* the row, so the cursor advances and space is reserved. In a right-to-left row it is
the right edge and the rect extends right, *outside* the row, so the cursor never advances
and nothing is reserved — the label then draws under the pin.

The patch makes the output reservation extend leftward into the row (`from_min_size(pos2(x
- spacing, y), (spacing, size))`), mirroring the input side, so output labels clear the
pins with the same gap. One block in `src/ui.rs`. The proper long-term fix is for snarl
to reserve the pin slot direction-correctly upstream.

## 4. `egui-snarl-wire-to-create.patch`

For wire-to-create (#123) the host needs to know which wire is currently armed (its source
pins), so it can press Space, open the node menu, create a node, and connect the armed wire
to it — and it needs a way to drop that wire once consumed. snarl keeps the armed wire in
its private `SnarlState`, not exposed to the viewer.

The patch adds two `SnarlViewer` hooks (both default no-ops):

- `report_new_wire(pins) -> bool`, called once per frame with the current armed wire's
  source pins (or `None`); returning `true` asks snarl to drop the wire that frame. Drives
  the Space wire-to-create path (arm a wire, press Space, pick a node).
- `on_wire_dropped(pos, pins)`, called when a wire is released on empty canvas, with the
  drop point (graph space) and source pins. Drives the drop wire-to-create path (drag a
  wire into empty space, let go, pick a node), so the host opens its own node menu there
  instead of snarl's dropped-wire context menu.

The trait methods live in `src/ui/viewer.rs`; the per-frame report and the drop call are in
`src/ui.rs`. The proper long-term fix is for snarl to expose the in-progress wire (and a
drop hook) upstream.

## 5. `egui-snarl-drop-on-wire.patch`

For drop-on-wire (#124) the host needs to know when a node is dropped on a wire, so it can
splice the node in (A -> B becomes A -> node -> B). snarl's own wire hover (`hovered_wire`,
via `hit_wire` against the cursor) cannot serve this: while a node is being dragged it is the
top widget, so the scene does not report the pointer as hovering it and the cursor-based hover
is suppressed. The hit-test must use the node's geometry, not the cursor.

The patch detects the drop in snarl, where both the node rect and the wire endpoints are
known: it notes when a node's own frame drag is released (`node_drag_released` on
`DrawNodeResponse`), then after the nodes are laid out it tests that node's centre against
each wire with `hit_wire`, and on a hit (where the node is not an endpoint) calls a new
`SnarlViewer::on_node_dropped_on_wire(node, out_pin, in_pin)` hook (default no-op). Pieces:
the trait method in `src/ui/viewer.rs`; the `node_drag_released` field, its capture in
`draw_node`, and the node-centre-vs-wire pass in `src/ui.rs`. The proper long-term fix is for
snarl to support node-on-wire splicing (or expose the hovered wire and node geometry)
upstream.

## Upgrading egui-snarl

When bumping the pinned version:

1. Re-vendor the new release into `vendor/egui-snarl/` (copy `src/`, `Cargo.toml`, and the
   `LICENSE-*` files from the new `egui-snarl-<version>` crate source under
   `~/.cargo/registry/src/`).
2. Re-apply the patches in order, from inside `vendor/egui-snarl/`:

   ```sh
   git apply -p1 ../../patches/egui-snarl-middle-pan.patch
   git apply -p1 ../../patches/egui-snarl-click-to-wire.patch
   git apply -p1 ../../patches/egui-snarl-output-pin-space.patch
   git apply -p1 ../../patches/egui-snarl-wire-to-create.patch
   git apply -p1 ../../patches/egui-snarl-drop-on-wire.patch
   ```

3. If a hunk rejects because upstream moved the surrounding code, apply that change by
   hand (each is small and self-contained), then regenerate its patch so it stays current.
   Generate each patch against the *previous* patches' result so they remain independent —
   for click-to-wire, diff a pristine-plus-middle-pan baseline against the vendored file
   (and for output-pin-space, a pristine-plus-middle-pan-plus-click-to-wire baseline):

   ```sh
   # baseline = pristine + middle-pan
   mkdir -p /tmp/snarl-base/src && cp <pristine ui.rs> /tmp/snarl-base/src/ui.rs
   ( cd /tmp/snarl-base && patch -p1 < patches/egui-snarl-middle-pan.patch )
   diff -u --label a/src/ui.rs --label b/src/ui.rs \
     /tmp/snarl-base/src/ui.rs vendor/egui-snarl/src/ui.rs \
     > patches/egui-snarl-click-to-wire.patch
   ```

4. Confirm the version in `vendor/egui-snarl/Cargo.toml` matches the pinned requirement,
   then `cargo build` to verify the patches took effect.
