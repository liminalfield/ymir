---
title: Project format
status: draft
---

# Project format

A Ymir project saves to a `.ymir` file (a `.json` extension opens too). It is plain JSON, kept readable and line-diffable so a project reads cleanly in version control.

A project has three parts.

`world` is what the graph builds: the seed, the world extent and height, the sea level, and the build resolution. These are the settings every node evaluates against, and they are what makes a project's terrain reproducible.

`graph` is the node network. Each node records its stable identity, its type, its parameters, and its connections, so a reopened project rebuilds to the same output it saved.

`view` is how the editor shows the project: where each node sits on the canvas, the canvas camera and frames, the preview resolution, the water rendering settings, and the node pane's ordering. None of it affects what the graph builds. A file with no `view` section still opens, with the nodes laid out automatically.

The file carries a `format_version`, and the graph inside it carries its own. The current project version is 2. Version 1 kept the world settings where only the editor could read them, so a version 1 project does not open in this build; it reports its version rather than loading part way.

A reusable node network saves on its own as a `.ymirsub` subgraph, with its own version, so it can be shared into another project's library.
