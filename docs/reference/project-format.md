---
title: Project format
status: draft
---

# Project format

A Ymir project saves to a `.ymir` file (a `.json` extension opens too). It is plain JSON, kept readable and line-diffable so a project reads cleanly in version control.

A project holds the node graph and the world settings. Each node records its stable identity, its type, its parameters, its connections, and its canvas position, so a reopened project rebuilds to the same output it saved. The world settings travel alongside the graph.

The file carries a `format_version`. The schema can still grow: a project saved by an older build keeps opening, and any field added since takes its default on load.

A reusable node network saves on its own as a `.ymirsub` subgraph, with its own version, so it can be shared into another project's library.
