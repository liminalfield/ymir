---
title: Export formats
status: draft
---

# Export formats

An export node at the end of a graph writes the height layer to a heightmap file. Three formats are available, each its own node. The two 16-bit formats map the height layer's actual range across the full output, so the whole bit depth carries detail wherever the terrain sits in the working range.

## PNG, 16-bit

[Export PNG](nodes/endpoint.export.md) writes a 16-bit grayscale PNG. It is widely readable and the common heightmap import format for game engines.

## R16, raw 16-bit

[Export R16](nodes/endpoint.export_r16.md) writes a raw 16-bit little-endian `.r16` file with no header, Unreal Engine's other native heightmap format. It uses the same range mapping as the PNG.

## EXR, 32-bit float

[Export EXR](nodes/endpoint.export_exr.md) writes a 32-bit float OpenEXR file. The float channel is lossless and writes the height values directly, with no range remap. It can bake absolute elevation in metres, as height times World height, so the file carries its own scale and needs nothing alongside it.
