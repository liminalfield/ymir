---
title: How erosion works with your terrain
status: draft
---

# How erosion works with your terrain

Erosion in Ymir reshapes the terrain you give it. It reads the height, moves material downhill, and writes the height back, so what it produces depends entirely on the form already there. It carves into existing shape rather than inventing shape of its own.

This is why erosion belongs after you have established the large forms, not before. Run it on a flat field and there is nothing for water or gravity to act on. Run it on a raw noise field and it will carve, but into noise, which reads as busy rather than natural. Give it real relief, ridgelines and valleys and a coast, and the erosion has something to work with: water gathers into the low ground, slopes shed toward their feet, and drainage networks appear where the terrain already slopes.

The three erosion nodes model different processes. Thermal erosion relaxes slopes steeper than their angle of repose, settling sharp ground into weathered slopes. Hydraulic erosion runs rain as droplets that cut rills and drop sediment into fans. Stream erosion carves the drainage network from where water accumulates. They read a mask, so you can hold erosion to the ground you choose.

Each writes its byproducts back as layers, `flow`, `wear`, and `deposition`, so a later node can drive texturing or further shaping from where the erosion did its work. A flow map is a product of erosion, not a property of raw noise: to get a believable one, feed the flow node terrain that has already been eroded.
