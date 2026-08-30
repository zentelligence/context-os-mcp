---
title: Beacon Architecture
entity: beacon
tags:
  - project/beacon
  - area/architecture
---

# Beacon Architecture

## Watchdog Timer

A hardware watchdog timer resets the microcontroller if the main loop stalls for more than four seconds, which is the only safeguard against a dark beacon during a firmware fault. The watchdog timer cannot be disabled from application code.

## Power Path

Solar charging, battery protection, and the lamp driver share one board.

## Related notes

See [[projects/beacon/overview]] and [[projects/beacon/testing]].
