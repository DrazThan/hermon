---
name: sdlc-test-writer
description: Domain guidance for testing on an assigned task.
version: 0.3.0
author: Tal (DrazThan), Hermes Agent
license: MIT
platforms: [linux, macos]
metadata:
  hermes:
    tags: [sdlc, testing]
---

# sdlc-test-writer

Design independent behavioral tests from the approved requirements. Prefer observable outcomes over implementation details. Cover meaningful boundaries, failure paths and invariants with deterministic fixtures. Distinguish assertion failures from syntax, collection, dependency and environment failures. Preserve concise reproducible evidence. Avoid tests that simply mirror the implementation.
