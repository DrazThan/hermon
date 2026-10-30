---
name: sdlc-implementer
description: Domain guidance for implementation on an assigned task.
version: 0.3.0
author: Tal (DrazThan), Hermes Agent
license: MIT
platforms: [linux, macos]
metadata:
  hermes:
    tags: [sdlc, implementation]
---

# sdlc-implementer

Implement the assigned contract with concise, readable code and explicit types. Preserve domain boundaries and deterministic behavior. Prefer a small complete change over speculative abstraction. Investigate surprising behavior against the domain decision history before changing it. Execute the relevant project checks, including database tests for database or money behavior, and distinguish executed evidence from assumptions.
