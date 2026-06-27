# Vendored dependency patches

We carry small patches to `egui-snarl` because parts of its interaction model are
hardcoded and not configurable through its public API. `egui-snarl` is vendored in
`vendor/egui-snarl/` and the workspace points at it via `[patch.crates-io]` in the root
`Cargo.toml`. Each patch below is also kept as a unified diff against the pristine crate
source, so the changes are reviewable and re-appliable on an upgrade.

The patches are independent and touch different regions of `src/ui.rs`; applied in order
(middle-pan, then click-to-wire) they reproduce the vendored source exactly.

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

## Upgrading egui-snarl

When bumping the pinned version:

1. Re-vendor the new release into `vendor/egui-snarl/` (copy `src/`, `Cargo.toml`, and the
   `LICENSE-*` files from the new `egui-snarl-<version>` crate source under
   `~/.cargo/registry/src/`).
2. Re-apply the patches in order, from inside `vendor/egui-snarl/`:

   ```sh
   git apply -p1 ../../patches/egui-snarl-middle-pan.patch
   git apply -p1 ../../patches/egui-snarl-click-to-wire.patch
   ```

3. If a hunk rejects because upstream moved the surrounding code, apply that change by
   hand (each is small and self-contained), then regenerate its patch so it stays current.
   Generate each patch against the *previous* patch's result so they remain independent —
   for click-to-wire, diff a pristine-plus-middle-pan baseline against the vendored file:

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
