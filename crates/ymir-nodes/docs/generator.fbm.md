---
status: draft
---

## Purpose

The workhorse noise generator: layered Perlin noise that reads as natural, rolling ground. Reach for it as the base for hills and undulating terrain, or as detail to add onto a larger shape.

Wavelength is a real size. A 500 m feature stays 500 m when the world extent changes, so growing the world gives you more ground at the same scale rather than the same terrain stretched over it. Octaves then add detail below that size, each one finer than the last by the lacunarity.
