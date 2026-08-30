---
title: Beacon Test Plan
entity: beacon
tags:
  - project/beacon
  - area/testing
---

# Beacon Test Plan

## Firmware Rollback Procedure

Every field update ships with a firmware rollback procedure: the previous image stays in the second flash bank so a failed update reverts within one watchdog cycle. The firmware rollback procedure is rehearsed on the bench before every release.

## Bench Tests

Power-cycle, brown-out, and thermal soak tests run before any field release.

## Field Tests

One unit runs a release candidate for two weeks before the fleet update.
