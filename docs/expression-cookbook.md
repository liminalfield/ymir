# Expression node cookbook

The Expression node evaluates one arithmetic formula per cell to produce the `height`
layer. It is the graph's escape hatch: a do-anything formula for when no small node fits,
or to prototype before a behaviour earns its own node. Use it sparingly, since logic in a
formula does not read from the wiring the way a node does.

This is a practical reference: the vocabulary first, then recipes. The most useful thing
it does is transform coordinates, which is lossless (nothing is resampled), so it is the
right tool for rotating, scaling, or offsetting a procedural pattern.

## What you can use

**Variables**

- `x`, `y`: the cell's world coordinates, `0..1` across the whole region.
- Input layers by name when the node is wired: `height`, `mask`, and any others the input
  carries (for example a flow field's `flow_x` / `flow_y`). An absent layer reads its
  default: `0`, or `1` for `mask`.

Unwired, only `x` and `y` are meaningful (the layers read their defaults), so the node acts
as a coordinate formula, a small generator. Wired, it transforms the input.

**Constants:** `pi`, `tau` (2 pi), `e`.

**Operators:** `+` `-` `*` `/` `^` (power) and unary `-`. Precedence, high to low:
`^` (right associative), unary `-`, then `*` `/`, then `+` `-`. So `-2^2` is `-(2^2)`.

**Functions**

| Function | Meaning |
| --- | --- |
| `sin(x)` `cos(x)` `tan(x)` | trigonometry, in radians |
| `abs(x)` `sign(x)` | magnitude, sign (-1 / 0 / 1) |
| `sqrt(x)` `exp(x)` `ln(x)` | root, e^x, natural log |
| `floor(x)` `ceil(x)` | round down / up |
| `min(a, b)` `max(a, b)` | smaller / larger |
| `pow(a, b)` | a to the power b (same as `a ^ b`) |
| `atan2(y, x)` | angle of the vector (x, y), in radians |
| `step(edge, x)` | `0` below `edge`, `1` at or above it |
| `clamp(x, lo, hi)` | hold `x` within `[lo, hi]` |
| `lerp(a, b, t)` | linear blend, `a + (b - a) * t` |
| `smoothstep(e0, e1, x)` | eased `0..1` ramp across `[e0, e1]` |
| `select(cond, a, b)` | `a` when `cond` is non-zero, else `b` |

`select` is the branch: the language has no `if`, so choose with `select` (or with
`step` / `min` / `max`). A non-finite result (a divide by zero, `sqrt` of a negative) is
written as `0`, so it cannot poison the field's range.

**One constraint to keep in mind:** there are no variables or intermediate assignments. A
formula is a single expression, and any parameter (an angle, a frequency) is a literal you
type into it and edit by hand. The recipes below inline their parameters for that reason.

## Coordinate transforms (the lossless ones)

The idea: `x` and `y` are coordinates, so transforming *them* transforms the pattern, with
no grid to resample and no edges to invent. Anything you generate from `x`/`y` can be
rotated, scaled, or offset exactly.

**Scale** is a multiply on the coordinate. Bigger number, finer pattern:

```
sin(x * 20)        // 20 ripples across the region
sin(x * 40)        // twice as fine
```

**Offset** is an add or subtract, sliding the pattern along an axis:

```
sin((x + 0.25) * 20)
```

**Rotate** the coordinate frame by an angle `a` in radians:

```
x' = x*cos(a) - y*sin(a)
y' = x*sin(a) + y*cos(a)
```

Write your pattern in terms of `x'` (and `y'` if it needs both), inlined. A pattern that
uses only `x'`, like a ripple, becomes a directional one you aim with `a`. Angles are in
radians; for degrees `d`, write `d * pi / 180` in place of `a`.

```
// Dunes rotated by 0.5 radians:
sin((x*cos(0.5) - y*sin(0.5)) * 20) * 0.1

// The same, expressed in degrees (30 deg):
sin((x*cos(30*pi/180) - y*sin(30*pi/180)) * 20) * 0.1
```

To pivot a rotation or scale about the region centre instead of the corner, subtract `0.5`
from the coordinate first and add it back. For a directional ripple the pivot only shifts
the phase, so it does not matter; it matters for radial patterns (below).

## Recipes

**Directional dunes / ripples** (amplitude `0.1`, frequency `20`, angle `0.5`):

```
sin((x*cos(0.5) - y*sin(0.5)) * 20) * 0.1
```

**Concentric rings** from the centre (radial, so pivot at `0.5`):

```
sin(sqrt((x-0.5)^2 + (y-0.5)^2) * 40)
```

**Terraces / steps** quantise a wired height into `n` flat levels (here `8`):

```
floor(height * 8) / 8
```

**Soft threshold** of the input around a level (hard with `step`, soft with `smoothstep`):

```
step(0.5, height)              // hard cut at 0.5
smoothstep(0.45, 0.55, height) // soft band
```

**Add detail into a base only where a mask allows** (wired, reads `height` and `mask`):

```
lerp(height, height + sin(x*60)*0.05, mask)
```

**Branchless conditional** with `select` (no `if` in the language):

```
select(step(0.5, height), height, height * 0.5)  // halve below 0.5, keep above
```

**Keep the result in range** by clamping the final value:

```
clamp(height * 1.5, 0, 1)
```

## When to reach for a node instead

If a transform is a property of the terrain (place this crater here, aim these dunes),
coordinate math here is the lossless home for it. If you find yourself rebuilding a whole
generator in a formula, that behaviour probably wants its own node. And to rotate a
*baked* field (a Blend output, an imported map), this node cannot help: that is a resample,
which the Import node's placement params or a future Transform node handle. See
`docs/design/transform-and-placement.md` for the full picture.
